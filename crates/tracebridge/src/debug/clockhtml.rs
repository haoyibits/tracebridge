//! `clock --html`: the clock tree as a diagram, one self-contained HTML file.
//!
//! The tree grows from the source clocks on the left to the clocks they feed
//! on the right, like the clock view of a vendor's configuration tool. The
//! browser lays it out (nested flex boxes, connectors drawn with borders), so
//! nothing here measures text. The file loads nothing from the network.

use std::fmt::Write;

use super::clock::{Evaluated, Report, State, format_frequency};

const STYLE: &str = include_str!("../../assets/clock.css");

/// Folds and unfolds the clocks that run from a clock.
const SCRIPT: &str = "\
document.addEventListener('click', (event) => {
  const button = event.target.closest('.fold');
  if (!button) return;
  const folded = button.closest('.node').classList.toggle('collapsed');
  button.textContent = folded ? '+' : '\u{2212}';
  button.setAttribute('aria-expanded', String(!folded));
});";

/// What the page says about the run.
pub struct Heading<'a> {
    /// The chip or the description's own title.
    pub title: &'a str,
    /// The description file.
    pub tree: &'a str,
    /// The frequencies the board gave: `XOSC = 40 MHz`.
    pub inputs: &'a [(String, f64)],
    /// Local time of the run.
    pub generated: &'a str,
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// `x20` and `/2` with the signs of a formula.
fn step(text: &str) -> String {
    match text.split_at_checked(1) {
        Some(("x", factor)) => format!("\u{00D7}{factor}"),
        Some(("/", factor)) => format!("\u{00F7}{factor}"),
        _ => text.to_string(),
    }
}

fn node(report: &Report, clock: &Evaluated, page: &mut String) {
    let (state, value, why) = match &clock.state {
        State::Hz(hz) => ("on", format_frequency(*hz), None),
        State::Off(reason) => ("off", "off".to_string(), Some(reason)),
        State::Unknown(reason) => ("unknown", "?".to_string(), Some(reason)),
        State::Error(reason) => ("error", "error".to_string(), Some(reason)),
    };
    let kind = if clock.root {
        " source"
    } else if clock.selection.is_some() {
        " mux"
    } else {
        ""
    };
    let children: Vec<&Evaluated> = report.children(Some(&clock.name)).collect();
    // Writing to a String cannot fail.
    let _ = write!(
        page,
        "<div class=\"node\"><div class=\"box {state}{kind}\">\
         <div class=\"head\"><span class=\"name\">{}</span><span class=\"freq\">{}</span></div>",
        escape(&clock.name),
        escape(&value)
    );
    if let Some(selection) = &clock.selection {
        let _ = write!(
            page,
            "<div class=\"how\">{} = {}</div><div class=\"opts\">",
            escape(&selection.field),
            selection.value
        );
        for (value, name) in &selection.options {
            let selected = if *value == selection.value {
                " sel"
            } else {
                ""
            };
            let _ = write!(
                page,
                "<span class=\"opt{selected}\">{value} {}</span>",
                escape(name)
            );
        }
        page.push_str("</div>");
    }
    if !clock.steps.is_empty() {
        let steps: Vec<String> = clock.steps.iter().map(|text| step(text)).collect();
        let _ = write!(
            page,
            "<div class=\"how\">{}</div>",
            escape(&steps.join(" "))
        );
    }
    if let Some(why) = why {
        let _ = write!(page, "<div class=\"why\">{}</div>", escape(why));
    }
    for warning in &clock.warnings {
        let _ = write!(page, "<div class=\"alert\">! {}</div>", escape(warning));
    }
    if let Some(note) = &clock.note {
        let _ = write!(page, "<div class=\"note\">{}</div>", escape(note));
    }
    if !children.is_empty() {
        let _ = write!(
            page,
            "<button class=\"fold\" type=\"button\" aria-expanded=\"true\" \
             title=\"Fold the clocks that run from {}\">\u{2212}</button>",
            escape(&clock.name)
        );
    }
    page.push_str("</div>");
    if !children.is_empty() {
        page.push_str("<div class=\"children\">");
        for child in children {
            node(report, child, page);
        }
        page.push_str("</div>");
    }
    page.push_str("</div>\n");
}

/// The whole page.
pub fn render(report: &Report, heading: &Heading) -> String {
    let mut page = String::new();
    let title = escape(heading.title);
    let _ = write!(
        page,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Clock tree: {title}</title>\n<style>\n{STYLE}</style>\n</head>\n<body>\n\
         <header>\n<h1>Clock tree <span>{title}</span></h1>\n"
    );
    let inputs: Vec<String> = heading
        .inputs
        .iter()
        .map(|(name, hz)| format!("<b>{} = {}</b>", escape(name), format_frequency(*hz)))
        .collect();
    let given = if inputs.is_empty() {
        "no frequency given".to_string()
    } else {
        format!("given {}", inputs.join(", "))
    };
    let _ = writeln!(
        page,
        "<p class=\"meta\">{given} &middot; read {} &middot; <code>{}</code> &middot; \
         tracebridge {}</p>",
        escape(heading.generated),
        escape(heading.tree),
        env!("CARGO_PKG_VERSION")
    );
    if !report.missing.is_empty() {
        let first = escape(&report.missing[0]);
        let _ = writeln!(
            page,
            "<p class=\"missing\">{} not given: run <code>tracebridge debug clock \
             {first}=&lt;frequency&gt; --html</code> or set it under [clock] in trace32.toml</p>",
            escape(&report.missing.join(", "))
        );
    }
    page.push_str(
        "<p class=\"legend\">\
         <span><i class=\"key source\"></i>source clock</span>\
         <span><i class=\"key mux\"></i>selector, the chosen source highlighted</span>\
         <span><i class=\"key\"></i>multiplier or divider</span>\
         <span><i class=\"key off\"></i>off</span>\
         <span><i class=\"key unknown\"></i>frequency unknown</span>\
         </p>\n</header>\n<main class=\"tree\">\n",
    );
    for root in report.children(None) {
        node(report, root, &mut page);
    }
    let _ = write!(
        page,
        "</main>\n<script>\n{SCRIPT}\n</script>\n</body>\n</html>\n"
    );
    page
}

/// A `file://` URL that a terminal makes clickable.
pub fn file_url(path: &std::path::Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                url.push(char::from(byte));
            }
            _ => {
                let _ = write!(url, "%{byte:02X}");
            }
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::clock::{evaluate, parse};
    use crate::debug::probe::fake::FakeProbe;
    use t32rcl::Value;

    fn page(description: &str, registers: &[(&str, i128)], inputs: &[(String, f64)]) -> String {
        let tree = parse(description).unwrap();
        let mut probe = FakeProbe::default();
        for (address, value) in registers {
            probe.set(&format!("Data.Long({address})"), Value::Int(*value));
        }
        let report = evaluate(&mut probe, &tree, inputs).unwrap();
        render(
            &report,
            &Heading {
                title: "Demo <chip>",
                tree: "/lib/demo.toml",
                inputs,
                generated: "2026-09-30 12:00",
            },
        )
    }

    const DEMO: &str = "\
[reg]
SEL = \"AD:0x10\"
DIV = \"AD:0x14\"
[[clock]]
name = \"OSC\"
note = \"crystal <8..40 MHz>\"
[[clock]]
name = \"IRC\"
hz = \"16MHz\"
[[clock]]
name = \"MUX\"
select = \"SEL[3:0]\"
sources = { \"0\" = \"IRC\", \"1\" = \"OSC\" }
[[clock]]
name = \"OUT\"
from = \"MUX\"
mul = \"3\"
div = \"DIV[3:0] + 1\"
enable = \"DIV[31]\"
warn = [{ when = \"DIV[8]\", text = \"test mode\" }]
[[clock]]
name = \"AUX\"
from = \"MUX\"
enable = \"DIV[30]\"
";

    #[test]
    fn page_shows_the_tree_with_its_states() {
        let inputs = [("OSC".to_string(), 8e6)];
        let html = page(DEMO, &[("AD:0x10", 1), ("AD:0x14", 0x8000_0103)], &inputs);
        assert!(
            html.starts_with("<!doctype html>\n<html lang=\"en\">"),
            "{html}"
        );
        assert!(html.contains("<title>Clock tree: Demo &lt;chip&gt;</title>"));
        assert!(html.contains("given <b>OSC = 8 MHz</b> &middot; read 2026-09-30 12:00"));
        // A source, with its note escaped.
        assert!(
            html.contains(
                "<div class=\"box on source\"><div class=\"head\"><span class=\"name\">OSC</span>\
             <span class=\"freq\">8 MHz</span></div><div class=\"how\">given</div>\
             <div class=\"note\">crystal &lt;8..40 MHz&gt;</div>"
            ),
            "{html}"
        );
        // The selector lists its sources; the chosen one is marked.
        assert!(
            html.contains(
                "<div class=\"box on mux\"><div class=\"head\"><span class=\"name\">MUX</span>\
             <span class=\"freq\">8 MHz</span></div><div class=\"how\">SEL[3:0] = 1</div>\
             <div class=\"opts\"><span class=\"opt\">0 IRC</span>\
             <span class=\"opt sel\">1 OSC</span></div>"
            ),
            "{html}"
        );
        // 8 MHz x 3 / 4, with the warning.
        assert!(
            html.contains(
                "<span class=\"name\">OUT</span><span class=\"freq\">6 MHz</span></div>\
             <div class=\"how\">\u{00D7}3 \u{00F7}4</div><div class=\"alert\">! test mode</div>"
            ),
            "{html}"
        );
        assert!(
            html.contains(
                "<div class=\"box off\"><div class=\"head\"><span class=\"name\">AUX</span>\
             <span class=\"freq\">off</span></div><div class=\"why\">DIV[30] = 0</div>"
            ),
            "{html}"
        );
        // MUX is under OSC, and OUT and AUX are under MUX: only clocks that
        // feed others can be folded.
        assert_eq!(html.matches("class=\"fold\"").count(), 2);
        assert_eq!(html.matches("<div").count(), html.matches("</div>").count());
        // Nothing is loaded from elsewhere.
        assert!(!html.contains("http"), "{html}");
        assert!(!html.contains("class=\"missing\""));
    }

    #[test]
    fn page_says_what_is_missing_or_not_described() {
        let html = page(DEMO, &[("AD:0x10", 7), ("AD:0x14", 0)], &[]);
        assert!(html.contains("<p class=\"meta\">no frequency given &middot;"));
        assert!(
            html.contains(
                "<p class=\"missing\">OSC not given: run <code>tracebridge debug clock \
             OSC=&lt;frequency&gt; --html</code>"
            ),
            "{html}"
        );
        assert!(html.contains(
            "<div class=\"box unknown source\"><div class=\"head\"><span class=\"name\">OSC</span>\
             <span class=\"freq\">?</span></div><div class=\"why\">frequency not given</div>"
        ), "{html}");
        // The selector shows a value that is none of its sources.
        assert!(
            html.contains(
                "<div class=\"how\">SEL[3:0] = 7</div><div class=\"opts\">\
             <span class=\"opt\">0 IRC</span><span class=\"opt\">1 OSC</span></div>\
             <div class=\"why\">this source is not described</div>"
            ),
            "{html}"
        );
    }

    #[test]
    fn file_urls_are_percent_encoded() {
        assert_eq!(
            file_url(std::path::Path::new(
                "/Users/me/My Project/.tracebridge/clock.html"
            )),
            "file:///Users/me/My%20Project/.tracebridge/clock.html"
        );
        assert_eq!(
            file_url(std::path::Path::new("/tmp/时钟.html")),
            "file:///tmp/%E6%97%B6%E9%92%9F.html"
        );
    }
}
