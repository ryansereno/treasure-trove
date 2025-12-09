use std::error::Error;
use std::fmt::Write;
use std::io::Write as IoWrite;
use std::process::{Command, Stdio};

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
use rusqlite::{Connection, Transaction, params};
use serde::Deserialize;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

fn init_db() -> rusqlite::Result<()> {
    let conn = Connection::open("inventory.db")?;

    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS containers (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL UNIQUE,   -- globally unique
            kind        TEXT,                   -- 'bin', 'drawer', etc. 
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS items (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            name          TEXT NOT NULL,
            quantity      INTEGER NOT NULL,
            container_id  INTEGER REFERENCES containers(id), -- nullable for loose items
            location_hint TEXT,                              
            created_at    TEXT NOT NULL DEFAULT (datetime('now'))
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
struct Container {
    id: i64,
    name: String,
    kind: Option<String>,
}

#[derive(Debug)]
struct Item {
    id: i64,
    name: String,
    quantity: i32,
    container_id: Option<i64>,
    location: Option<String>,
}

#[derive(Deserialize)]
struct InputForm {
    text: String,
    container_select: Option<String>,
    container_new: Option<String>,
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
    let container_list = match load_containers() {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Failed to load containers: {e}");
            Vec::new()
        }
    };

    let mut html = String::new();

    html.push_str(r#"<!doctype html>
        <html lang="en">
          <head>
            <meta charset="utf-8">
            <title>Treasure Trove</title>
            <meta name="viewport" content="width=device-width, initial-scale=1.0">
          </head>
          <body style="font-family: serif; padding: 1rem; justify-self: center; max-width: 600px;">
          <div style="display:flex; justify-content:space-between; font-size:0.9rem;">
            <img src="/static/logo.jpg" alt="Logo" style="height: 60px; display:block; margin-bottom:1rem;">
              <a href="/items" style="margin-bottom: 1rem; display: inline-block;">View Trove</a>
              </div>
            <p style="font-size: 0.8rem; color: gray; font-style: italic; margin-top: -1rem; margin-bottom: 1rem; ">
            A household ledger for tools and treasures.
              </p>
            <form method="post" action="/submit">
              <label for="text">Speak or list your treasures:</label><br>
              <textarea id="text" name="text" rows="8" cols="40" style="width: 100%;"></textarea><br><br>
        "#);

    html.push_str(
        r#"<label for="container_select">Select Container (optional):</label><br>
      <select id="container_select" name="container_select" style="width: 100%;">"#,
    );

    html.push_str(r#"<option value="">-- None --</option>"#);
    for c in &container_list {
        let escaped = html_escape(&c.name);
        html.push_str(&format!(
            r#"<option value="{id}">{label}</option>"#,
            id = c.id,
            label = escaped,
        ));
    }
    html.push_str("</select><br><br>");

    html.push_str(
        r#"<label for="container_new">New Bin (if Other or new):</label><br>
      <input id="container_new" name="container_new" type="text" style="width: 100%;" /><br><br>
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
    let parsed_items = match llm_parse(&input.text).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("LLM parse failed: {e}");
            vec![ParsedItem {
                name: input.text.trim().to_string(),
                quantity: 1,
            }]
        }
    };
    //need to convert container_select from Option<String> to Option<i64>
    let container_select_id: Option<i64> = input
        .container_select
        .as_deref() // &str
        .and_then(|s| {
            let t = s.trim();
            if t.is_empty() {
                None // treat "" as None
            } else {
                t.parse::<i64>().ok() // invalid numbers -> None
            }
        });

    let container_new = input.container_new;
    let location = normalize_optional(input.location);

    let mut items: Vec<Item> = Vec::new();

    {
        let mut conn = Connection::open("inventory.db").expect("failed to open DB");
        let tx = conn.transaction().expect("failed to start transaction");

        let container_id = match choose_container(&tx, container_select_id, container_new) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Failed to resolve container: {e}");
                None
            }
        };

        items = parsed_items
            .into_iter()
            .map(|pi| Item {
                id: 0,
                name: pi.name,
                quantity: pi.quantity,
                container_id,
                location: location.clone(),
            })
            .collect();

        if let Err(e) = save_items_tx(&tx, &items) {
            eprintln!("Failed to save items: {e}");
        }

        tx.commit().expect("failed to commit transaction");
    }

    if let Err(e) = print_zebra_label(&items) {
        eprintln!("Failed to print zebra label: {e}")
    }

    let mut html = String::new();
    html.push_str(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Inventory Saved</title>",
    );
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\"></head><body style=\"font-family: sans-serif; padding: 1rem;\">");
    html.push_str("<h1>Parsed Items</h1><ul>");

    for item in &items {
        let mut line = format!("{} × {}", item.quantity, html_escape(&item.name));

        if let Some(container_id) = item.container_id {
            line.push_str(&format!(" — Container ID: {}", container_id));
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

async fn llm_parse(raw: &str) -> Result<Vec<ParsedItem>, Box<dyn Error + Send + Sync>> {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("LLM Parse called");
    println!("Raw input text:\n{}", raw);
    let ollama = Ollama::default();

    let prompt = format!(
        r#"
        You parse messy inventory text into structured data.
        
        Input text will be multiple items, sometimes separated by commas or newlines but not always.        Example:
        
          3 boxes of nails, hammer 10 screws
          couple of wrenchs
        
        Rules:
        - Split the text into separate items.
        - Each item must have:
          - quantity: integer >= 1
          - name: short name for the object (no extra commentary)
        - If the item includes a number or mentions a vague quantity, use that as quantity.
        - Otherwise, default quantity to 1.
        
        Return ONLY JSON, no explanations, exactly in this shape:
        
        {{
          "items": [
            {{ "name": "box of nails", "quantity": 3 }},
            {{ "name": "hammer", "quantity": 1 }},
            {{ "name": "screws", "quantity": 10 }},
            {{ "name": "wrench", "quantity": 2 }}
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
    Ok(parsed.items)
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
              <body style="font-family: serif; padding: 1rem; max-width: 400px; margin: 0 auto;">
                <h1 style="font-size: 1.4rem; margin-bottom: 0.75rem;">Inventory</h1>
        "#,
    );

    if items.is_empty() {
        html.push_str("<p><em>No items yet.</em></p>\n");
    } else {
        let mut current_heading: Option<String> = None;
        let mut box_open = false;

        for row in items {
            let heading = match &row.container_name {
                Some(name) => format!("Container: {}", name),
                None => "Loose items (no container)".to_string(),
            };

            if current_heading.as_deref() != Some(heading.as_str()) {
                if box_open {
                    html.push_str("      </tbody></table></div>\n");
                }

                html.push_str(
                    r#"<div style="border: 1px solid black; padding: 0.5rem; margin-bottom: 0.75rem;">
                    <div style="font-weight: bold; font-size: 0.9rem; margin-bottom: 0.25rem;">"#,
                );
                html.push_str(&html_escape(&heading));
                html.push_str(
                    r#"</div>
                        <table style="width: 100%; border-collapse: collapse; font-size: 0.9rem;">
                        <tbody>
                    "#,
                );

                box_open = true;
                current_heading = Some(heading);
            }

            let item = row.item;
            let mut line = format!("{} × {}", item.quantity, html_escape(&item.name));

            if let Some(ref loc) = item.location {
                line.push_str(&format!(" — {}", html_escape(loc)));
            }

            html.push_str(
                r#"      <tr>
                    <td style="padding: 2px 4px; border-top: 1px solid #eee;">"#,
            );
            html.push_str(&line);
            html.push_str("</td>\n      </tr>\n");
        }

        if box_open {
            html.push_str("    </tbody></table></div>\n");
        }
    }

    html.push_str(
        r#"    <p style="margin-top: 1rem;"><a href="/">Back to form</a></p>
            </body>
        </html>"#,
    );

    Html(html)
}

#[derive(Debug)]
struct ItemWithContainer {
    item: Item,
    container_name: Option<String>,
}

fn load_items_from_db() -> rusqlite::Result<Vec<ItemWithContainer>> {
    let conn = Connection::open("inventory.db")?;

    let mut stmt = conn.prepare(
        r#"
        SELECT 
            i.id,
            i.name,
            i.quantity,
            i.container_id,
            i.location_hint,
            c.name
        FROM items i
        LEFT JOIN containers c ON i.container_id = c.id
        ORDER BY 
            c.name IS NULL,    -- containers first, loose items last
            c.name ASC,
            datetime(i.created_at) DESC
        "#,
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(ItemWithContainer {
            item: Item {
                id: row.get(0)?,
                name: row.get(1)?,
                quantity: row.get(2)?,
                container_id: row.get(3)?,
                location: row.get(4)?,
            },
            container_name: row.get(5)?,
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

fn load_containers() -> rusqlite::Result<Vec<Container>> {
    let conn = Connection::open("inventory.db")?;
    let mut stmt = conn.prepare("SELECT id, name, kind FROM containers ORDER BY name")?;

    let rows = stmt.query_map([], |row| {
        Ok(Container {
            id: row.get(0)?,
            name: row.get(1)?,
            kind: row.get(2)?,
        })
    })?;

    let mut containers = Vec::new();
    for r in rows {
        containers.push(r?);
    }
    Ok(containers)
}

fn save_items_tx(tx: &rusqlite::Transaction, items: &[Item]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO items (name, quantity, container_id, location_hint)
         VALUES (?1, ?2, ?3, ?4)",
    )?;

    for item in items {
        stmt.execute(params![
            &item.name,
            item.quantity,
            item.container_id,
            &item.location,
        ])?;
    }

    Ok(())
}

fn print_zebra_label(items: &[Item]) -> Result<(), Box<dyn std::error::Error>> {
    let mut zpl_body = String::new();
    let mut y = 20;

    for item in items {
        zpl_body.push_str(&format!(
            "^FO20,{}^ADN^FD{} x {}^FS\n",
            y,
            item.quantity,
            item.name
        ));
        y += 22; 
    }

    let zpl = format!(
        "^XA\
        ^PW812\
        ^LH0,0\
        {body}\
        ^XZ",
        body = zpl_body
    );

    const PRINTER_NAME: &str = "zebra";

    let mut child = std::process::Command::new("lp")
        .arg("-d")
        .arg(PRINTER_NAME)
        .arg("-o")
        .arg("raw")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(stdin) = &mut child.stdin {
        stdin.write_all(zpl.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        eprintln!("CUPS exited with status {status}");
    }

    Ok(())
}

fn choose_container(
    tx: &Transaction,
    container_select: Option<i64>,
    container_new: Option<String>,
) -> rusqlite::Result<Option<i64>> {
    if let Some(new_name) = normalize_optional(container_new) {
        // Insert if it doesn't exist
        tx.execute(
            "INSERT OR IGNORE INTO containers (name) VALUES (?1)",
            params![&new_name],
        )?;

        // Fetch id for that name
        let id: i64 = tx.query_row(
            "SELECT id FROM containers WHERE name = ?1",
            params![&new_name],
            |row| row.get(0),
        )?;

        return Ok(Some(id));
    }

    // Otherwise, use selected id (or None)
    Ok(container_select)
}

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
