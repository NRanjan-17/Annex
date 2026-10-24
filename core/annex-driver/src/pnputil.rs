//! Reading `pnputil /enum-drivers` output. Its labels ("Published Name", "Veröffentlichter
//! Name", ...) follow the display language, so only the values are matched.

/// Published names (`oemNN.inf`) of driver-store entries whose original name is `original`.
/// Entries are blank-line-separated blocks; the one naming `original` holds its `oemNN.inf`.
pub fn published_names(pnputil_output: &str, original: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in pnputil_output.lines().chain([""]) {
        if !line.trim().is_empty() {
            block.extend(tokens(line));
            continue;
        }
        if block.iter().any(|t| t.eq_ignore_ascii_case(original))
            && let Some(p) = block.iter().find(|t| is_published_name(t))
        {
            names.push(p.to_string());
        }
        block.clear();
    }
    names
}

fn tokens(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| c.is_whitespace() || c == ':').filter(|t| !t.is_empty())
}

/// `oem` + digits + `.inf`, any case.
fn is_published_name(token: &str) -> bool {
    let t = token.to_ascii_lowercase();
    t.strip_prefix("oem")
        .and_then(|r| r.strip_suffix(".inf"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_english_listing() {
        let out = "Microsoft PnP Utility\r\n\r\nPublished Name:     oem12.inf\r\nOriginal Name:      annexidd.inf\r\nProvider Name:      Annex\r\nClass Name:         Display adapters\r\n\r\nPublished Name: oem3.inf\r\nOriginal Name: other.inf\r\n";
        assert_eq!(published_names(out, "annexidd.inf"), vec!["oem12.inf"]);
    }

    #[test]
    fn parses_german_listing() {
        let out = "Microsoft-PnP-Dienstprogramm\n\nVeröffentlichter Name:  oem7.inf\nUrsprünglicher Name:    other.inf\nAnbietername:           Contoso\n\nVeröffentlichter Name:  oem42.inf\nUrsprünglicher Name:    AnnexIdd.inf\nAnbietername:           Annex\nKlassenname:            Grafikkarten\n";
        assert_eq!(published_names(out, "annexidd.inf"), vec!["oem42.inf"]);
    }

    #[test]
    fn several_packages() {
        let out = "Published Name: oem1.inf\nOriginal Name: annexidd.inf\n\n\nPublished Name: oem2.inf\nOriginal Name: annexidd.inf\n";
        assert_eq!(published_names(out, "annexidd.inf"), vec!["oem1.inf", "oem2.inf"]);
    }
}
