use std::io::Read;
use std::time::Instant;
fn rss_kb() -> u64 { std::fs::read_to_string("/proc/self/status").unwrap().lines().find(|l| l.starts_with("VmHWM")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0) }
fn run<F: FnOnce() -> String + std::panic::UnwindSafe>(name: &str, f: F) {
    let t = Instant::now();
    let r = std::panic::catch_unwind(f);
    let out = match r { Ok(s) => s, Err(_) => "PANIC".into() };
    let s: String = out.chars().take(900).collect::<String>().replace('\n', "⏎");
    println!("{name:34} {:>6} ms  peakRSS {:>6} MB | {s}", t.elapsed().as_millis(), rss_kb()/1024);
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let what = a[1].as_str(); let p = a[2].clone();
    match what {
        "lopdf" => run(&format!("lopdf {p}"), move || {
            let d = match lopdf::Document::load(&p) { Ok(d) => d, Err(e) => return format!("ERR load: {e}") };
            let pages = d.get_pages();
            let nums: Vec<u32> = pages.keys().cloned().collect();
            let enc = d.is_encrypted();
            match d.extract_text(&nums) { Ok(t) => format!("pages={} encrypted={enc} text={t}", pages.len()), Err(e) => format!("pages={} encrypted={enc} ERR extract: {e}", pages.len()) }
        }),
        "oxide" => run(&format!("pdf_oxide {p}"), move || {
            let mut d = match pdf_oxide::PdfDocument::open(&p) { Ok(d) => d, Err(e) => return format!("ERR open: {e}") };
            let n = d.page_count().unwrap_or(0);
            let mut out = String::new();
            for i in 0..n { match d.extract_text(i) { Ok(t) => { out.push_str(&t); } Err(e) => out.push_str(&format!("[ERR p{i}: {e}]")) } }
            format!("pages={n} text={out}")
        }),
        "spans" => run("pdf_oxide spans", move || {
            let d = pdf_oxide::PdfDocument::open(&p).unwrap();
            let sp = d.extract_spans(0).unwrap();
            sp.iter().take(6).map(|s| format!("{:?}@{:?}", s.text, s.bbox)).collect::<Vec<_>>().join(" | ") + &format!(" ... total {}", sp.len())
        }),
        "scancheck" => run("scan heuristic", move || {
            let d = lopdf::Document::load(&p).unwrap();
            let mut out = String::new();
            for (n, id) in d.get_pages() {
                let imgs = d.get_page_images(id).map(|v| v.len()).unwrap_or(0);
                let txt = d.extract_text(&[n]).unwrap_or_default();
                out.push_str(&format!("page {n}: images={imgs} text_chars={} -> {}; ", txt.trim().len(), if imgs > 0 && txt.trim().is_empty() { "NO_TEXT_LAYER(image only)" } else { "has text" }));
            }
            out
        }),
        "enccheck" => run("encryption", move || {
            let d = pdf_oxide::PdfDocument::open(&p).unwrap();
            format!("pdf_oxide.is_encrypted={}", d.is_encrypted())
        }),
        "zipmeta" => run("zip meta", move || {
            let f = std::fs::File::open(&p).unwrap();
            let mut z = match zip::ZipArchive::new(f) { Ok(z) => z, Err(e) => return format!("ERR zip: {e}") };
            let n = z.len();
            let mut names = Vec::new();
            for i in 0..n.min(5) { if let Ok(e) = z.by_index_raw(i) { names.push(format!("{:?}(enclosed={:?})", e.name(), e.enclosed_name().is_some())); } }
            let by = z.by_name("word/document.xml").map(|mut e| { let mut s = String::new(); let _ = e.read_to_string(&mut s); s.chars().skip(s.find("<w:t>").unwrap_or(0)).take(30).collect::<String>() }).unwrap_or_else(|e| format!("ERR {e}"));
            format!("entries={n} first={} by_name(word/document.xml)={}", names.join(","), by)
        }),
        "xmlopts" => run("roxmltree", move || {
            let f = std::fs::File::open(&p).unwrap();
            let mut z = zip::ZipArchive::new(f).unwrap();
            let mut buf = String::new();
            z.by_name("word/document.xml").unwrap().take(16<<20).read_to_string(&mut buf).unwrap();
            let def = match roxmltree::Document::parse(&buf) { Ok(_) => "parsed".to_string(), Err(e) => format!("REFUSED: {e}") };
            let mut o = roxmltree::ParsingOptions::default(); o.allow_dtd = true; o.nodes_limit = 100_000;
            let dtd = match roxmltree::Document::parse_with_options(&buf, o) { Ok(d) => { let t: usize = d.descendants().filter_map(|n| n.text()).map(|t| t.len()).sum(); format!("parsed with DTD allowed; text bytes={t}") } Err(e) => format!("REFUSED: {e}") };
            format!("default: {def} | allow_dtd+nodes_limit: {dtd}")
        }),
        "docx" => run(&format!("zip+roxmltree {p}"), move || {
            let f = std::fs::File::open(&p).unwrap();
            let mut z = match zip::ZipArchive::new(f) { Ok(z) => z, Err(e) => return format!("ERR zip: {e}") };
            let mut f = match z.by_name("word/document.xml") { Ok(f) => f, Err(e) => return format!("ERR entry: {e}") };
            let declared = f.size();
            const CAP: u64 = 16 * 1024 * 1024;
            let mut buf = String::new();
            let n = (&mut f).take(CAP + 1).read_to_string(&mut buf).unwrap_or(0) as u64;
            if n > CAP { return format!("REFUSED at cap {CAP} (declared uncompressed {declared})"); }
            let doc = match roxmltree::Document::parse(&buf) { Ok(d) => d, Err(e) => return format!("ERR xml: {e}") };
            let mut out = String::new();
            for p in doc.descendants().filter(|n| n.has_tag_name("p")) {
                let style = p.descendants().find(|n| n.has_tag_name("pStyle")).and_then(|n| n.attributes().next().map(|a| a.value().to_string())).unwrap_or_default();
                let t: String = p.descendants().filter(|n| n.has_tag_name("t")).filter_map(|n| n.text()).collect();
                out.push_str(&format!("[{style}]{t}|"));
            }
            let tbl = doc.descendants().filter(|n| n.has_tag_name("tbl")).count();
            format!("declared={declared} paragraphs: {out} tables={tbl}")
        }),
        "html" => run(&format!("scraper {p}"), move || {
            let s = std::fs::read_to_string(&p).unwrap();
            let h = scraper::Html::parse_document(&s);
            let sel = scraper::Selector::parse("h1,h2,p,td,li,title").unwrap();
            h.select(&sel).map(|e| format!("<{}>{}", e.value().name(), e.text().collect::<String>())).collect::<Vec<_>>().join("|")
        }),
        _ => {}
    }
}
