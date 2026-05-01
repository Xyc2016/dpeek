use owo_colors::OwoColorize;
use regex::Regex;

pub fn rich_highlight(text: &str) -> String {
    let re = Regex::new(include_str!("../repr_highlighter.regex")).unwrap();

    re.replace_all(text, |caps: &regex::Captures| {
        if let Some(m) = caps.name("tag_name")         { m.as_str().bright_magenta().bold().to_string() }
        else if let Some(m) = caps.name("tag_start")    { m.as_str().bold().to_string() }
        else if let Some(m) = caps.name("tag_end")      { m.as_str().bold().to_string() }
        else if let Some(m) = caps.name("attrib_name")  { m.as_str().yellow().to_string() }
        else if let Some(m) = caps.name("attrib_value") { m.as_str().magenta().to_string() }
        else if let Some(m) = caps.name("brace")        { m.as_str().bold().to_string() }
        else if let Some(m) = caps.name("bool_true")    { m.as_str().bright_green().italic().to_string() }
        else if let Some(m) = caps.name("bool_false")   { m.as_str().bright_red().italic().to_string() }
        else if let Some(m) = caps.name("none")         { m.as_str().magenta().italic().to_string() }
        else if let Some(m) = caps.name("ellipsis")     { m.as_str().yellow().to_string() }
        else if let Some(m) = caps.name("ipv4")         { m.as_str().bright_green().bold().to_string() }
        else if let Some(m) = caps.name("ipv6")         { m.as_str().bright_green().bold().to_string() }
        else if let Some(m) = caps.name("eui48")        { m.as_str().bright_green().bold().to_string() }
        else if let Some(m) = caps.name("eui64")        { m.as_str().bright_green().bold().to_string() }
        else if let Some(m) = caps.name("uuid")         { m.as_str().bright_yellow().to_string() }
        else if let Some(m) = caps.name("number")       { m.as_str().cyan().bold().to_string() }
        else if let Some(m) = caps.name("number_complex") { m.as_str().cyan().bold().to_string() }
        else if let Some(m) = caps.name("str")          { m.as_str().green().to_string() }
        else if let Some(m) = caps.name("url")          { m.as_str().bright_blue().underline().to_string() }
        else if let Some(m) = caps.name("path")         { m.as_str().magenta().to_string() }
        else if let Some(m) = caps.name("filename")     { m.as_str().bright_magenta().to_string() }
        else if let Some(m) = caps.name("call")         { m.as_str().magenta().bold().to_string() }
        else { caps.get(0).unwrap().as_str().to_string() }
    }).to_string()
}
