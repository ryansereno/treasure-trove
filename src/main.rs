use std::error::Error;

use axum::{
    Router,
    extract::Form,
    response::Html,
    routing::{get, post},
};
use ollama_rs::{
    Ollama,
    generation::{completion::request::GenerationRequest, parameters::FormatType},
};
use rusqlite::{Connection, params};
use serde::Deserialize;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

fn init_db() -> rusqlite::Result<()> {
    let conn = Connection::open("inventory.db")?;

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS items (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            name      TEXT NOT NULL,
            quantity  INTEGER NOT NULL,
            bin_id    TEXT,
            location  TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    )?;

    Ok(())
}

#[tokio::main]
async fn main() {
    init_db().expect("failed to initialize database");

    let app = Router::new()
        .route("/", get(show_form))
        .route("/submit", post(handle_submit))
        .route("/items", get(show_items))
        .nest_service("/static", ServeDir::new("static"));

    let listener = TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to address");

    println!("Server running on http://localhost:3000");
    axum::serve(listener, app).await.expect("server error");
}

#[derive(Debug)]
struct Item {
    name: String,
    quantity: i32,
    bin_id: Option<String>,
    location: Option<String>,
}

#[derive(Deserialize)]
struct InputForm {
    text: String,
    bin_select: Option<String>,
    bin_new: Option<String>,
    location: Option<String>,
}
#[derive(Debug, Deserialize)]
struct ParsedInventory {
    items: Vec<ParsedItem>,
}

#[derive(Debug, Deserialize)]
struct ParsedItem {
    name: String,
    quantity: i32,
}

async fn show_form() -> Html<String> {
    let known_bins = known_bins();

    let mut html = String::new();

    html.push_str(r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Treasure Trove</title>
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
  </head>
  <body style="font-family: serif; padding: 1rem; justify-self: center; max-width: 600px;">
    <img src="/static/logo.jpg" alt="Logo" style="height: 60px; display:block; margin-bottom:1rem;">
    <p style="font-size: 0.8rem; color: gray; font-style: italic; margin-top: -1rem; margin-bottom: 1rem; ">
    A household ledger for tools and treasures.
      </p>
    <form method="post" action="/submit">
      <label for="text">Speak or list your treasures:</label><br>
      <textarea id="text" name="text" rows="8" cols="40" style="width: 100%;"></textarea><br><br>
"#);

    html.push_str(
        r#"<label for="bin_select">Select Bin (optional):</label><br>
      <select id="bin_select" name="bin_select" style="width: 100%;">"#,
    );

    html.push_str(r#"<option value="">-- None --</option>"#);
    for bin in known_bins {
        let escaped = html_escape(bin);
        html.push_str(&format!(
            r#"<option value="{value}">{label}</option>"#,
            value = escaped,
            label = escaped
        ));
    }
    html.push_str("</select><br><br>");

    html.push_str(
        r#"<label for="bin_new">New Bin (if Other or new):</label><br>
      <input id="bin_new" name="bin_new" type="text" style="width: 100%;" /><br><br>
"#,
    );

    html.push_str(
        r#"<label for="location">Location (optional):</label><br>
      <input id="location" name="location" type="text" style="width: 100%;" /><br><br>
"#,
    );

    html.push_str(
        r#"      <button type="submit">Submit</button>
    </form>
  </body>
</html>"#,
    );

    Html(html)
}

async fn handle_submit(Form(input): Form<InputForm>) -> Html<String> {
    // Resolve bin selection: new bin beats select; "-" and empty -> None
    let bin_id = choose_bin(input.bin_select, input.bin_new);
    let location = normalize_optional(input.location);

    let items = match llm_parse(&input.text, bin_id.clone(), location.clone()).await {
        Ok(items) => items,
        Err(e) => {
            eprintln!("LLM parse failed: {e}");
            eprintln!("Storing as single raw entry");

            vec![Item {
                name: input.text.trim().to_string(),
                quantity: 1,
                bin_id: bin_id.clone(),
                location: location.clone(),
            }]
        }
    };

    if let Err(e) = save_items_to_db(&items) {
        eprintln!("Failed to save to DB: {e}");
    }

    // Render confirmation page
    let mut html = String::new();
    html.push_str(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Inventory Saved</title>",
    );
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\"></head><body style=\"font-family: sans-serif; padding: 1rem;\">");
    html.push_str("<h1>Parsed Items</h1><ul>");

    for item in &items {
        let mut line = format!("{} × {}", item.quantity, html_escape(&item.name));

        if let Some(ref bin) = item.bin_id {
            line.push_str(&format!(" — Bin: {}", html_escape(bin)));
        }
        if let Some(ref loc) = item.location {
            line.push_str(&format!(" — Location: {}", html_escape(loc)));
        }

        html.push_str("<li>");
        html.push_str(&line);
        html.push_str("</li>");
    }

    html.push_str("</ul>");
    html.push_str(r#"<p><a href="/">Back</a></p>"#);
    html.push_str("</body></html>");

    Html(html)
}

async fn llm_parse(
    raw: &str,
    bin_id: Option<String>,
    location: Option<String>,
) -> Result<Vec<Item>, Box<dyn Error + Send + Sync>> {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("LLM Parse called");
    println!("Raw input text:\n{}", raw);
    println!("Bin ID: {:?}", bin_id);
    println!("Location: {:?}", location);
    let ollama = Ollama::default();

    let prompt = format!(
        r#"
You parse messy inventory text into structured data.

Input text will be multiple lines like:

  3 boxes of nails
  hammer
  10 screws

Rules:
- Split the text into separate items.
- Each item must have:
  - quantity: integer >= 1
  - name: short name for the object (no extra commentary)
- If the line starts with a number, use that as quantity.
- Otherwise, default quantity to 1.

Return ONLY JSON, no explanations, exactly in this shape:

{{
  "items": [
    {{ "name": "hammer", "quantity": 1 }},
    {{ "name": "box of nails", "quantity": 3 }}
  ]
}}

Now parse this input:

\"\"\"{raw}\"\"\" 
"#,
    );

    println!("Sending prompt to Ollama:\n{}", prompt);

    let request = GenerationRequest::new("gemma3:1b".to_string(), prompt).format(FormatType::Json);

    println!("Calling Ollama (model: gemma3:1b)...");

    let res = ollama.generate(request).await?;

    println!("Ollama responded!");
    println!("Raw response:\n{}", res.response);

    let parsed: ParsedInventory = serde_json::from_str(&res.response)?;

    let items = parsed
        .items
        .into_iter()
        .map(|pi| Item {
            name: pi.name,
            quantity: pi.quantity,
            bin_id: bin_id.clone(),
            location: location.clone(),
        })
        .collect();

    Ok(items)
}

fn save_items_to_db(items: &[Item]) -> rusqlite::Result<()> {
    let mut conn = Connection::open("inventory.db")?;
    let tx = conn.transaction()?;

    {
        let mut stmt = tx.prepare(
            "INSERT INTO items (name, quantity, bin_id, location)
             VALUES (?1, ?2, ?3, ?4)",
        )?;

        for item in items {
            stmt.execute(params![
                item.name,
                item.quantity,
                item.bin_id,
                item.location,
            ])?;
        }
    }

    tx.commit()?;
    Ok(())
}

async fn show_items() -> Html<String> {
    let items = match load_items_from_db() {
        Ok(items) => items,
        Err(e) => {
            eprintln!("Failed to load items: {e}");
            Vec::new()
        }
    };

    let mut html = String::new();

    html.push_str(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Inventory</title>
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
  </head>
  <body style="font-family: serif; padding: 1rem; max-width: 600px;">
    <h1>Inventory</h1>
    <ul>
"#,
    );

    for item in items {
        let mut line = format!("{} × {}", item.quantity, html_escape(&item.name));

        if let Some(ref bin) = item.bin_id {
            line.push_str(&format!(" — Bin: {}", html_escape(bin)));
        }
        if let Some(ref loc) = item.location {
            line.push_str(&format!(" — Location: {}", html_escape(loc)));
        }

        html.push_str("<li>");
        html.push_str(&line);
        html.push_str("</li>");
    }

    html.push_str(
        r#"    </ul>
    <p><a href="/">Back to form</a></p>
  </body>
</html>"#,
    );

    Html(html)
}

fn load_items_from_db() -> rusqlite::Result<Vec<Item>> {
    let conn = Connection::open("inventory.db")?;

    let mut stmt = conn.prepare(
        r#"
        SELECT name, quantity, bin_id, location
        FROM items
        ORDER BY datetime(created_at) DESC
        "#,
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(Item {
            name: row.get(0)?,
            quantity: row.get(1)?,
            bin_id: row.get(2)?,
            location: row.get(3)?,
        })
    })?;

    let mut items = Vec::new();
    for row_result in rows {
        items.push(row_result?);
    }

    Ok(items)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// Hardcoded list of known bins for now
fn known_bins() -> &'static [&'static str] {
    &[
        "Spring 1",
        "Spring 2",
        "Autumn",
        "Tapes & Adhesives",
        "Wires & Cables",
    ]
}

fn choose_bin(bin_select: Option<String>, bin_new: Option<String>) -> Option<String> {
    // If user typed a new bin, that wins
    if let Some(new) = normalize_optional(bin_new) {
        return Some(new);
    }

    // Otherwise, use selected bin if it's not empty/"-"
    if let Some(sel) = bin_select {
        let trimmed = sel.trim();
        if !trimmed.is_empty() && trimmed != "-" {
            return Some(trimmed.to_string());
        }
    }

    None
}

// Normalize empty strings to None
fn normalize_optional(opt: Option<String>) -> Option<String> {
    opt.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}
