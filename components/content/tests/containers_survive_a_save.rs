//! A table or list that holds a link or an image is not lost on save
//! (ADR-059): parse, serialize, parse again, and the block is still there.

use content::{parse_document, serialize_document, assign_slots, ContentElement};
use schema::Grammar;

const SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n----\n\nBody.\nheadings\n: h2..h6\n";

fn grammar() -> Grammar {
    schema::parse_schema(SCHEMA).expect("schema parses")
}

fn body_after_save(src: &str) -> (Vec<ContentElement>, String) {
    let g = grammar();
    let doc = assign_slots(&parse_document(src).expect("parses"), &g);
    let saved = serialize_document(&doc);
    let again = assign_slots(&parse_document(&saved).expect("parses again"), &g);
    (again.body.iter().map(|e| e.node.clone()).collect(), saved)
}

#[test]
fn a_table_with_a_link_survives_a_save() {
    let src = "# T\n\n----\n\n## Fns\n\n| Function | Meaning |\n| --- | --- |\n| `juxt` | see [Template composition](#template-composition) |\n| `pipe` | plain |\n";
    let (body, saved) = body_after_save(src);
    #[allow(deprecated)] // stage 6.3
    let table = body.iter().find_map(|e| e.table_cell_sources());
    let (headers, rows) = table.unwrap_or_else(|| panic!("the table was lost; saved:\n{saved}\n{body:#?}"));
    assert_eq!(headers, vec!["Function".to_string(), "Meaning".to_string()]);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0][1], "see [Template composition](#template-composition)");
    assert_eq!(rows[1], vec!["`pipe`".to_string(), "plain".to_string()]);
    assert!(saved.contains("[Template composition](#template-composition)"), "{saved}");
}

#[test]
fn a_list_with_an_image_survives_a_save() {
    let src = "# T\n\n----\n\n- ![i](a.png)\n- b [c](/c)\n";
    let (body, saved) = body_after_save(src);
    assert_eq!(body.len(), 1, "{body:#?}");
    match &body[0] {
        ContentElement::List { source } => {
            assert!(source.contains("![i](a.png)") && source.contains("[c](/c)"), "{source}");
        }
        other => panic!("expected a list, got {other:?}; saved:\n{saved}"),
    }
}
