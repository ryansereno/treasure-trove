use axum::{
    Router,
    extract::Form,
    response::Html,
    routing::{get, post},
};
use serde::Deserialize;
use std::{fs::OpenOptions, io::Write};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(show_form))
        .route("/submit", post(handle_submit))
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

    let items = fake_llm_parse(&input.text, bin_id.clone(), location.clone());

    if let Err(e) = append_items_to_csv("inventory.csv", &items) {
        eprintln!("Failed to write CSV: {e}");
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

fn fake_llm_parse(raw: &str, bin_id: Option<String>, location: Option<String>) -> Vec<Item> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut parts = line.split_whitespace();
            let first = parts.next().unwrap_or("");

            if let Ok(qty) = first
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse::<i32>()
            {
                let name = parts.collect::<Vec<_>>().join(" ");
                Item {
                    name: if name.is_empty() {
                        line.to_string()
                    } else {
                        name
                    },
                    quantity: qty,
                    bin_id: bin_id.clone(),
                    location: location.clone(),
                }
            } else {
                Item {
                    name: line.to_string(),
                    quantity: 1,
                    bin_id: bin_id.clone(),
                    location: location.clone(),
                }
            }
        })
        .collect()
}

fn append_items_to_csv(path: &str, items: &[Item]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    for item in items {
        writeln!(
            file,
            "{},{},{},{}",
            item.quantity,
            item.name,
            item.bin_id.as_deref().unwrap_or(""),
            item.location.as_deref().unwrap_or("")
        )?;
    }

    Ok(())
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
