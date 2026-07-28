//! `cargo run -p tohdr-conformance --example dump -- <file>` — the parsed
//! container, for when a criterion's verdict needs explaining.

use tohdr_conformance::isobmff::{Heif, Prop};

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump <file>");
    let bytes = std::fs::read(&path).expect("read");
    let f = Heif::parse(&bytes).expect("parse");
    println!("brands: {:?}", f.brands);
    println!("pitm: {:?}", f.primary);
    for i in &f.items {
        let props: Vec<String> = f
            .props_of(i.id)
            .iter()
            .map(|p| match p {
                Prop::Ispe { width, height } => format!("ispe {width}x{height}"),
                Prop::Pixi { bits } => format!("pixi {bits:?}"),
                Prop::AuxC { urn } => format!("auxC {urn}"),
                Prop::Other(t) => String::from_utf8_lossy(t).into_owned(),
            })
            .collect();
        let data = f.item_data(i.id);
        println!(
            "item {:>3} {:<5} hidden={:<5} ct={:<22} data={:<28} {}",
            i.id,
            i.typ,
            i.hidden,
            i.content_type,
            match &data {
                Ok(d) => format!("{} bytes", d.len()),
                Err(e) => format!("ERR {e}"),
            },
            props.join(", ")
        );
        if i.content_type == "application/rdf+xml"
            && let Ok(d) = &data
        {
            let text = String::from_utf8_lossy(d);
            for line in text.lines().filter(|l| l.contains("HDR")) {
                println!("        {}", line.trim());
            }
        }
    }
    for r in &f.refs {
        println!("ref {} {} -> {:?}", String::from_utf8_lossy(&r.typ), r.from, r.to);
    }
    for g in &f.groups {
        println!("group {} id={} {:?}", String::from_utf8_lossy(&g.typ), g.id, g.entities);
    }
}
