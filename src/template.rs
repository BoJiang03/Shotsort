//! Folder / file-name template rendering.
//!
//! Supported tokens: `{YYYY} {YY} {MM} {DD} {HH} {mm} {ss} {original} {ext}`
//! `{counter} {counter:03} {camera_make} {camera_model}`.

use chrono::{Datelike, NaiveDateTime, Timelike};

/// Values available while rendering a template.
pub struct Ctx<'a> {
    pub dt: Option<NaiveDateTime>,
    /// Original file stem (no extension).
    pub original: &'a str,
    /// Original extension (no dot).
    pub ext: &'a str,
    pub counter: Option<u32>,
    pub make: &'a str,
    pub model: &'a str,
}

/// Render a template string. Unknown tokens are passed through verbatim so a
/// typo is visible rather than silently dropped.
pub fn render(tpl: &str, ctx: &Ctx) -> String {
    let mut out = String::with_capacity(tpl.len() + 8);
    let mut rest = tpl;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let token = &after[..close];
                out.push_str(&render_token(token, ctx));
                rest = &after[close + 1..];
            }
            None => {
                // Unbalanced brace: emit the rest literally.
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn render_token(tok: &str, ctx: &Ctx) -> String {
    let (name, spec) = match tok.split_once(':') {
        Some((a, b)) => (a, Some(b)),
        None => (tok, None),
    };
    let d = ctx.dt;
    match name {
        "YYYY" => d.map(|x| format!("{:04}", x.year())).unwrap_or_default(),
        "YY" => d
            .map(|x| format!("{:02}", x.year().rem_euclid(100)))
            .unwrap_or_default(),
        "MM" => d.map(|x| format!("{:02}", x.month())).unwrap_or_default(),
        "DD" => d.map(|x| format!("{:02}", x.day())).unwrap_or_default(),
        "HH" => d.map(|x| format!("{:02}", x.hour())).unwrap_or_default(),
        "mm" => d.map(|x| format!("{:02}", x.minute())).unwrap_or_default(),
        "ss" => d.map(|x| format!("{:02}", x.second())).unwrap_or_default(),
        "original" => ctx.original.to_string(),
        "ext" => ctx.ext.to_string(),
        "camera_make" => sanitize_value(ctx.make),
        "camera_model" => sanitize_value(ctx.model),
        "counter" => {
            let c = ctx.counter.unwrap_or(0);
            let width = spec.map(parse_width).unwrap_or(0);
            format!("{:0width$}", c, width = width)
        }
        _ => format!("{{{tok}}}"),
    }
}

fn parse_width(spec: &str) -> usize {
    spec.trim().parse::<usize>().unwrap_or(spec.len())
}

/// Sanitize a *token value* (e.g. camera model) so it cannot inject path
/// separators or characters illegal on FAT/exFAT.
fn sanitize_value(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect();
    cleaned.trim().trim_matches('.').trim().to_string()
}

/// Sanitize a single path component produced by a folder template (keeps the
/// component intact but strips illegal characters; `/` separators are handled
/// by the caller splitting on them first).
pub fn sanitize_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 6, 20)
            .unwrap()
            .and_hms_opt(9, 5, 3)
            .unwrap()
    }

    fn ctx<'a>(c: Option<u32>) -> Ctx<'a> {
        Ctx {
            dt: Some(dt()),
            original: "DSC00001",
            ext: "ARW",
            counter: c,
            make: "SONY",
            model: "ILCE-7M4",
        }
    }

    #[test]
    fn folder_default() {
        assert_eq!(
            render("{YYYY}/{YYYY}-{MM}-{DD}", &ctx(None)),
            "2026/2026-06-20"
        );
    }

    #[test]
    fn readable_name() {
        assert_eq!(
            render("{YYYY}{MM}{DD}_{HH}{mm}{ss}", &ctx(None)),
            "20260620_090503"
        );
    }

    #[test]
    fn original_token() {
        assert_eq!(render("{original}", &ctx(None)), "DSC00001");
    }

    #[test]
    fn counter_padding() {
        assert_eq!(render("{counter:03}", &ctx(Some(7))), "007");
        assert_eq!(render("{counter}", &ctx(Some(7))), "7");
    }

    #[test]
    fn unknown_token_passthrough() {
        assert_eq!(render("{nope}", &ctx(None)), "{nope}");
    }

    #[test]
    fn missing_date_blank() {
        let c = Ctx {
            dt: None,
            ..ctx(None)
        };
        assert_eq!(render("{YYYY}-{MM}", &c), "-");
    }
}
