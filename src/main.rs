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
    //    let parsed_items = vec![
    //        ParsedItem {
    //            name: "chuck with extra jaws".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "chuck tightening rods".into(),
    //            quantity: 2,
    //        },
    //        ParsedItem {
    //            name: "small faceplate".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "live centers".into(),
    //            quantity: 2,
    //        },
    //        ParsedItem {
    //            name: "dead center".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "set of carbide turning tools".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "roughing gouges".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "skew chisel".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "parting tools".into(),
    //            quantity: 2,
    //        },
    //        ParsedItem {
    //            name: "calipers".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "sandpaper".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "box of pen blanks".into(),
    //            quantity: 1,
    //        },
    //        ParsedItem {
    //            name: "mandrel for pens".into(),
    //            quantity: 1,
    //        },
    //    ];
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
    let mut container_name: Option<String> = None;

    {
        let mut conn = Connection::open("inventory.db").expect("failed to open DB");
        let tx = conn.transaction().expect("failed to start transaction");

        let (container_id, name_opt) =
            match choose_container(&tx, container_select_id, container_new) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("Failed to resolve container: {e}");
                    (None, None)
                }
            };
        container_name = name_opt;

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

    if let Err(e) = print_zebra_label(&items, container_name.as_deref()) {
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

fn print_zebra_label(
    items: &[Item],
    container_name: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let header = match container_name {
        Some(name) => name,
        None => "",
    };
    let mut zpl_body = String::new();
    let mut y = 80;

    for item in items {
        zpl_body.push_str(&format!(
            "^FO40,{}^FB525,3,0,L,0^ADN^FD{} x {}^FS\n",
            y, item.quantity, item.name
        ));

        y += 22;
    }

    let zpl = format!(
        "^XA\
        ^PW812\
        ^LH0,0\
        ^FO560,150^GFA,2875,2875,25,,:::::J03FFE0IF8K03I03NF89S04,J07gXF80FFC,J0LF87LF8007F71IF3QFCMF8,J0LF07E01E0ER08J07JFC07E003FC,J0E7FF1FJ03E0ER08J063IFC07EI018,J06I03FJ0EF03J0DM0C8I0C3FFCF87F00E1,J06007FF800F07818I0D9K01CB00183F8I07IFE3,J0703IF80780781CI07FI0807FE00307FEI078I03,J0307IFCI01FC0EI03E001801FE00607IF007FI07,J038E0F9CI031E07007FE041800FF80C0FE0300FFE00E,J03801E0EI0E1F0380IFC61F01BE01C1FEJ0F87C0C,J0180301F00383F80C03FFE3BC03F80383FFI01F80F1C,J01C3C07F80607FC0603F601F80E4C0607E38001FE0018,K0C001C78180E3C03027207F808440C0FE1C003FF0018,K0E00703C70183E01803207FE0080381FF06007C7C03,K0600C03C00603F00E01201838080703F38180F80F03,K070700EE00C0C78018I01J081C07F8E063F80386,K03800387030183E00F8003K0700FFC7003FE01C4,K03800707860303F003EM01E03FCE1807E3800C,K01C01C0FC8060C7C007M0F007F870C07E1F808,L0C0701CE018187F001F8J07C00FFC3860FF03E18,L0E04071FI0307BC003EI0FE003F8E0C00FFC0018,L06I0E3F80060C1F8001IFJ07F860601F9F003,L03001873C00C181BFO01F0C30303FC7806,L018030E3F0183031F8N0FF871800F9E1C06,L0180C1C7F83060211FCK01FF9830C01F87060C,M0C0030E7C20C0631FFEI01IF9C18603FC1800C,M04006183EJ0C613FFC3JFB8C0C2079E0E018,M06018307EI0184233KFE71860400F8E07038,M03I0608FI010C621JFEE30C30201FC70387,M01I0C10FC0021846363F18630410103DC3800E,N0801820BEI01084263B0821860800FCE1C00C,N04030411F800218426330C3083I01FC70E01C,N06020831FC00410844330C10C1I03FC306038,N038010617EI02084433041041I0F8E38307,N01C020C21F800610843184186I01F861C10E,K06I0E041861FF00400843186082I0FFC60C01C,K0CI070010C17FF08008C318208I03F9C306038,J01CI038021823IF0010C30820C001FF0E18307,J0388001C001061IF80308308004007E3861C30E,J0398I0E0060C33FFCI08M01E61870C11C0033,J0618I0700C0821IFE008M0FE60C3060180073,J0E3J0380018431IFO03FE30C10303800C6,J0C3J01E003082187FCM03FE6386180060018C,J086K0F0020863063JF801FFEE73861800C00318,K0CK0380010C20C3OFC631C6080380023,001818K01C003084187OF8631C30C06I02,007EN0700210C1071LF9FC631C3040C,00FF8M0380421830E00KF9FC218E1843800E,00C3E0CL0E0041861F007JF8FE2186I07001F84,00E0F0CL038083041FF01JFCFE3082001C0031CE,00E07FCL01E002083BFC0JFCE6308200380030EF,00C01F8M0380600707F01FFCC6210C2006I020FF,K02O0E0C00F81F80FC8E6318I01CI0207E,U070801FF07F07CCE731J07L03E,U01F803FFE0F8FCC6711I03C,V07C0783F87DFCC6311I0E,K0EP01F1F80FE3FFC4639101F8,00180FQ07FF801F1FEC661900F8,00381FQ01IF0070FE446I0FC,00381BS0FFC039FCK0FCP078,00183T0E1F01FFE8041FEN0181F8,00383S01C0FC0MFEM060301FC,00383S03803F0FF7IFCN0F070398,00383S0EI078FES0F8E038,00383R038I03CFCS0DCC03,00383R0EJ01FFCT0F806,003831P038K0FF8T07806,003833P0EL0FFU07806,003833O03CK01FEU0F806,00303FO078K03FCU0FC06,00303EO0EJ0303F8T01CE06,002018N038J07077U038F07,T07K070EV03870318,T0E01I071EV070783B8,S01C01800E1CV0E0381F8,S03I0C01E3CU01E03C0E,S06I0C07C78U01C01C,S0CI0C3F8F8U01C,R018I0C7F1F,R018300C7C1E,R030701CF03C,R060E009F078,R061E389E0F,R0C3C799E1F,Q01C7C799E1E,Q018F8F31E38,Q031F9D23FF,Q021B1927FE,Q06323B3FFC,Q06723B1FFC,Q0C667103F8,Q09C4E187F,P0198CE1FF7,P01F09C0EE6,P01E19C006E,P0181B800FE,R01B800FC,R01FI0FC,R01EI0F8,R03CI0F8,R01J0F,V01F,:W0E,W04,,::^FS\
        ^FO40,40^FB525,3,0,L,0^AEN,20,20^FD{header}^FS\
        {body}\
        ^XZ",
        body = zpl_body
    );
    println!("Generated ZPL:\n{}", zpl);
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
) -> rusqlite::Result<(Option<i64>, Option<String>)> {
    if let Some(new_name) = normalize_optional(container_new) {
        //insert new container if not exists
        tx.execute(
            "INSERT OR IGNORE INTO containers (name) VALUES (?1)",
            params![&new_name],
        )?;

        let id: i64 = tx.query_row(
            "SELECT id FROM containers WHERE name = ?1",
            params![&new_name],
            |row| row.get(0),
        )?;

        return Ok((Some(id), Some(new_name)));
    }

    if let Some(id) = container_select {
        //lookup container name
        let name: String = tx.query_row(
            "SELECT name FROM containers WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;

        return Ok((Some(id), Some(name)));
    }
    //no container selected or created
    Ok((None, None))
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
