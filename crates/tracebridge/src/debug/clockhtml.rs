//! `clock --html`: the clock tree as a diagram, one self-contained HTML file.
//!
//! Two views of the same clocks:
//!
//! - **By module**, when the description names groups: one panel per group,
//!   as a reference manual draws a clock generation module. Every selector
//!   starts a row with all its inputs on the left, the chosen one marked;
//!   what it feeds follows on the right. A signal that comes from another
//!   row is named, not wired, and the name links to where it is made.
//! - **By source**: one tree, every clock under the clock it runs from now.
//!   Its connectors are the paths in use and are drawn bold; a selector also
//!   names the inputs it does not use, joined to it with thin lines.
//!
//! The browser lays both out (nested flex boxes, connectors drawn with
//! borders), so nothing here measures text. The file loads nothing from the
//! network.

use std::fmt::Write;

use super::clock::{Evaluated, Report, State, format_frequency};

const STYLE: &str = include_str!("../../assets/clock.css");

/// Folds branches and switches the view.
const SCRIPT: &str = include_str!("../../assets/clock.js");

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

#[derive(Clone, Copy, PartialEq)]
enum View {
    Modules,
    Tree,
}

impl View {
    /// Every clock has a box in each view; this tells their ids apart.
    fn prefix(self) -> &'static str {
        match self {
            View::Modules => "m-",
            View::Tree => "t-",
        }
    }
}

/// The frequency, or the word for the state.
fn value(clock: &Evaluated) -> String {
    match &clock.state {
        State::Hz(hz) => format_frequency(*hz),
        State::Off(_) => "off".to_string(),
        State::Unknown(_) => "?".to_string(),
        State::Error(_) => "error".to_string(),
    }
}

fn find<'a>(report: &'a Report, name: &str) -> Option<&'a Evaluated> {
    report.clocks.iter().find(|clock| clock.name == name)
}

/// In the module view a clock starts a row of its own when it is a source
/// or a selector, or when what feeds it belongs to another group.
fn starts_row(report: &Report, clock: &Evaluated) -> bool {
    match clock.parent.as_deref().and_then(|name| find(report, name)) {
        None => true,
        Some(parent) => clock.selection.is_some() || parent.group != clock.group,
    }
}

/// The clocks drawn to the right of `clock`.
fn fed<'a>(report: &'a Report, clock: &'a Evaluated, view: View) -> Vec<&'a Evaluated> {
    report
        .children(Some(&clock.name))
        .filter(|child| view == View::Tree || !starts_row(report, child))
        .collect()
}

fn node(report: &Report, clock: &Evaluated, view: View, page: &mut String) {
    let (state, why) = match &clock.state {
        State::Hz(_) => ("on", None),
        State::Off(reason) => ("off", Some(reason)),
        State::Unknown(reason) => ("unknown", Some(reason)),
        State::Error(reason) => ("error", Some(reason)),
    };
    let kind = if clock.root {
        " source"
    } else if clock.selection.is_some() {
        " mux"
    } else {
        ""
    };
    // Where the links of the inputs lead, in each view.
    let prefix = view.prefix();
    let children = fed(report, clock, view);
    page.push_str("<div class=\"node\">");
    // In the tree the input in use is the connector that arrives at the
    // box; the inputs the selector does not use are named in front of it.
    if let (View::Tree, Some(selection)) = (view, &clock.selection) {
        let chosen = clock.parent.is_some();
        let _ = write!(
            page,
            "<div class=\"alts{}\">",
            if chosen { "" } else { " none" }
        );
        for (code, name) in &selection.options {
            if *code != selection.value {
                input(report, view, Some(*code), name, false, page);
            }
        }
        page.push_str("</div>");
    }
    // Writing to a String cannot fail.
    let _ = write!(
        page,
        "<div class=\"box {state}{kind}\" id=\"{prefix}{0}\">\
         <div class=\"head\"><span class=\"name\">{0}</span><span class=\"freq\">{1}</span></div>",
        escape(&clock.name),
        escape(&value(clock))
    );
    if let Some(selection) = &clock.selection {
        let _ = write!(
            page,
            "<div class=\"how\">{} = {}</div>",
            escape(&selection.field),
            selection.value
        );
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
            node(report, child, view, page);
        }
        page.push_str("</div>");
    }
    page.push_str("</div>\n");
}

/// One input of a row: the selector value that picks it, the clock's name
/// as a link to where it is made, and its frequency.
fn input(
    report: &Report,
    view: View,
    code: Option<u64>,
    name: &str,
    selected: bool,
    page: &mut String,
) {
    let class = if selected { "in sel" } else { "in" };
    let prefix = view.prefix();
    let code = code
        .map(|code| format!("<span class=\"code\">{code}</span>"))
        .unwrap_or_default();
    let frequency = find(report, name).map(value).unwrap_or_default();
    let _ = write!(
        page,
        "<a class=\"{class}\" href=\"#{prefix}{0}\">{code}<span>{0}</span><b>{1}</b></a>",
        escape(name),
        escape(&frequency)
    );
}

/// A row of the module view: the inputs, then the clock and what it feeds.
fn row(report: &Report, clock: &Evaluated, page: &mut String) {
    page.push_str("<div class=\"row\">");
    if let Some(selection) = &clock.selection {
        page.push_str("<div class=\"inputs\">");
        for (code, name) in &selection.options {
            input(
                report,
                View::Modules,
                Some(*code),
                name,
                *code == selection.value,
                page,
            );
        }
        if !selection
            .options
            .iter()
            .any(|(code, _)| *code == selection.value)
        {
            let _ = write!(
                page,
                "<span class=\"in sel unknown\"><span class=\"code\">{}</span>\
                 <span>not described</span><b>?</b></span>",
                selection.value
            );
        }
        page.push_str("</div>");
    } else if let Some(parent) = &clock.parent {
        page.push_str("<div class=\"inputs\">");
        input(report, View::Modules, None, parent, true, page);
        page.push_str("</div>");
    }
    node(report, clock, View::Modules, page);
    page.push_str("</div>\n");
}

/// The module view: a panel per group, in the order the groups appear.
fn modules(report: &Report, page: &mut String) {
    let mut groups: Vec<&Option<String>> = Vec::new();
    for clock in &report.clocks {
        if !groups.contains(&&clock.group) {
            groups.push(&clock.group);
        }
    }
    page.push_str("<main class=\"modules\">\n");
    for group in groups {
        let title = group.as_deref().unwrap_or("Other clocks");
        let _ = writeln!(page, "<section class=\"panel\"><h2>{}</h2>", escape(title));
        for clock in &report.clocks {
            if &clock.group == group && starts_row(report, clock) {
                row(report, clock, page);
            }
        }
        page.push_str("</section>\n");
    }
    page.push_str("</main>\n");
}

/// The whole page.
pub fn render(report: &Report, heading: &Heading) -> String {
    // Without groups there are no modules to draw.
    let grouped = report.clocks.iter().any(|clock| clock.group.is_some());
    let view = if grouped { "modules" } else { "tree" };
    let mut page = String::new();
    let title = escape(heading.title);
    let _ = write!(
        page,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Clock tree: {title}</title>\n<style>\n{STYLE}</style>\n</head>\n\
         <body data-view=\"{view}\">\n<header>\n<h1>Clock tree <span>{title}</span></h1>\n"
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
    if grouped {
        page.push_str(
            "<p class=\"views\">\
             <button type=\"button\" data-view=\"modules\" aria-pressed=\"true\">By module</button>\
             <button type=\"button\" data-view=\"tree\" aria-pressed=\"false\">By source</button>\
             </p>\n",
        );
    }
    page.push_str(
        "<p class=\"legend\">\
         <span><i class=\"key source\"></i>source clock</span>\
         <span><i class=\"key mux\"></i>selector, the chosen source highlighted</span>\
         <span><i class=\"key\"></i>multiplier or divider</span>\
         <span><i class=\"key off\"></i>off</span>\
         <span><i class=\"key unknown\"></i>frequency unknown</span>\
         </p>\n\
         <p class=\"legend lines\">\
         <span><i class=\"stroke\"></i>the path in use</span>\
         <span><i class=\"stroke thin\"></i>an input the selector does not use</span>\
         </p>\n</header>\n",
    );
    if grouped {
        modules(report, &mut page);
    }
    page.push_str("<main class=\"tree\">\n");
    for root in report.children(None) {
        node(report, root, View::Tree, &mut page);
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
                "<div class=\"box on source\" id=\"t-OSC\"><div class=\"head\"><span class=\"name\">OSC</span>\
             <span class=\"freq\">8 MHz</span></div><div class=\"how\">given</div>\
             <div class=\"note\">crystal &lt;8..40 MHz&gt;</div>"
            ),
            "{html}"
        );
        // The selector is drawn under the source in use (OSC); the input it
        // does not use is named in front of its box, with a link to the
        // box that makes it.
        assert!(
            html.contains(
                "<div class=\"children\"><div class=\"node\"><div class=\"alts\">\
             <a class=\"in\" href=\"#t-IRC\"><span class=\"code\">0</span><span>IRC</span><b>16 MHz</b></a>\
             </div><div class=\"box on mux\" id=\"t-MUX\"><div class=\"head\">\
             <span class=\"name\">MUX</span>\
             <span class=\"freq\">8 MHz</span></div><div class=\"how\">SEL[3:0] = 1</div>"
            ),
            "{html}"
        );
        assert_eq!(html.matches("class=\"alts").count(), 1);
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
                "<div class=\"box off\" id=\"t-AUX\"><div class=\"head\"><span class=\"name\">AUX</span>\
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
        // No groups in the description: the tree only, and no switch.
        assert!(html.contains("<body data-view=\"tree\">"), "{html}");
        assert!(!html.contains("class=\"modules\""));
        assert!(!html.contains("class=\"views\""));
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
        assert!(
            html.contains(
                "<div class=\"box unknown source\" id=\"t-OSC\"><div class=\"head\">\
             <span class=\"name\">OSC</span>\
             <span class=\"freq\">?</span></div><div class=\"why\">frequency not given</div>"
            ),
            "{html}"
        );
        // The selector shows a value that is none of its sources: no input
        // is in use, so both are named and no connector arrives.
        assert!(
            html.contains(
                "<div class=\"node\"><div class=\"alts none\">\
             <a class=\"in\" href=\"#t-IRC\"><span class=\"code\">0</span><span>IRC</span><b>16 MHz</b></a>\
             <a class=\"in\" href=\"#t-OSC\"><span class=\"code\">1</span><span>OSC</span><b>?</b></a>\
             </div><div class=\"box unknown mux\" id=\"t-MUX\"><div class=\"head\">\
             <span class=\"name\">MUX</span><span class=\"freq\">?</span></div>\
             <div class=\"how\">SEL[3:0] = 7</div>\
             <div class=\"why\">this source is not described</div>"
            ),
            "{html}"
        );
    }

    /// DEMO with the oscillators in one group and the rest in another.
    fn grouped() -> String {
        let mut description = DEMO.to_string();
        for (name, group) in [
            ("OSC", "Oscillators"),
            ("IRC", "Oscillators"),
            ("MUX", "Outputs"),
            ("OUT", "Outputs"),
            ("AUX", "Spare"),
        ] {
            description = description.replace(
                &format!("name = \"{name}\"\n"),
                &format!("name = \"{name}\"\ngroup = \"{group}\"\n"),
            );
        }
        description
    }

    #[test]
    fn groups_are_drawn_as_modules() {
        let inputs = [("OSC".to_string(), 8e6)];
        let html = page(
            &grouped(),
            &[("AD:0x10", 1), ("AD:0x14", 0xC000_0103)],
            &inputs,
        );
        // The module view comes first and is the one shown.
        assert!(html.contains("<body data-view=\"modules\">"), "{html}");
        assert!(html.contains(
            "<button type=\"button\" data-view=\"modules\" aria-pressed=\"true\">By module</button>"
        ));
        let (modules, tree) = html.split_once("<main class=\"tree\">").unwrap();
        let titles: Vec<&str> = modules
            .split("<h2>")
            .skip(1)
            .map(|rest| rest.split("</h2>").next().unwrap())
            .collect();
        assert_eq!(titles, ["Oscillators", "Outputs", "Spare"]);
        // The selector's row: every source as an input, the chosen one
        // marked, each a link to the box that makes it.
        assert!(modules.contains(
            "<div class=\"row\"><div class=\"inputs\">\
             <a class=\"in\" href=\"#m-IRC\"><span class=\"code\">0</span><span>IRC</span><b>16 MHz</b></a>\
             <a class=\"in sel\" href=\"#m-OSC\"><span class=\"code\">1</span><span>OSC</span><b>8 MHz</b></a>\
             </div><div class=\"node\"><div class=\"box on mux\" id=\"m-MUX\">"
        ), "{modules}");
        // The tree draws the input in use as its connector and names only
        // the other one; its links stay within the tree.
        assert!(!modules.contains("class=\"alts"), "{modules}");
        assert!(
            tree.contains(
                "<div class=\"alts\"><a class=\"in\" href=\"#t-IRC\"><span class=\"code\">0</span>\
                 <span>IRC</span><b>16 MHz</b></a></div><div class=\"box on mux\" id=\"t-MUX\">"
            ),
            "{tree}"
        );
        assert!(!tree.contains("#m-"), "{tree}");
        // OUT stays next to MUX; AUX is in another group, so it starts a
        // row that names its source.
        assert!(
            modules.contains(
                "<div class=\"children\"><div class=\"node\"><div class=\"box on\" id=\"m-OUT\">"
            ),
            "{modules}"
        );
        assert!(
            modules.contains(
                "<div class=\"row\"><div class=\"inputs\">\
             <a class=\"in sel\" href=\"#m-MUX\"><span>MUX</span><b>8 MHz</b></a>\
             </div><div class=\"node\"><div class=\"box on\" id=\"m-AUX\">"
            ),
            "{modules}"
        );
        // A source clock has no inputs.
        assert!(
            modules.contains(
                "<div class=\"row\"><div class=\"node\"><div class=\"box on source\" id=\"m-OSC\">"
            ),
            "{modules}"
        );
        // Every link has its target, once in each view.
        for name in ["OSC", "IRC", "MUX", "OUT", "AUX"] {
            for prefix in ["m-", "t-"] {
                assert_eq!(
                    html.matches(&format!("id=\"{prefix}{name}\"")).count(),
                    1,
                    "{prefix}{name}"
                );
            }
        }
        assert_eq!(html.matches("<div").count(), html.matches("</div>").count());
    }

    #[test]
    fn a_selector_value_without_a_source_is_an_input_too() {
        let html = page(&grouped(), &[("AD:0x10", 7), ("AD:0x14", 0)], &[]);
        assert!(html.contains(
            "<a class=\"in\" href=\"#m-OSC\"><span class=\"code\">1</span><span>OSC</span><b>?</b></a>\
             <span class=\"in sel unknown\"><span class=\"code\">7</span>\
             <span>not described</span><b>?</b></span></div>"
        ), "{html}");
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
