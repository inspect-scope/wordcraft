use super::options::PdfOptions;
use super::{DateTime, Document, Metadata};

pub(crate) fn metadata(doc: &Document, opts: &PdfOptions) -> Metadata {
    let c = &doc.core;
    let mut m = Metadata { creator: "WordCraft".into(), producer: "WordCraft".into(), ..Default::default() };
    let title = opts.title.clone().unwrap_or_else(|| c.title.clone());
    if !title.trim().is_empty() {
        m.title = Some(title);
    }
    let author = opts.author.clone().unwrap_or_else(|| c.creator.clone());
    if !author.trim().is_empty() {
        m.authors = vec![author];
    }
    if !c.subject.trim().is_empty() {
        m.description = Some(c.subject.clone());
    } else if !c.description.trim().is_empty() {
        m.description = Some(c.description.clone());
    }
    let kw: Vec<String> = c.keywords.split([',', ';']).map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
    if !kw.is_empty() {
        m.keywords = kw;
    }
    if let Some((y, mo, d, h, mi, s)) = parse_iso(&c.created) {
        m.created =
            Some(DateTime::new(y.clamp(0, 9999) as u16).month(mo).day(d).hour(h).minute(mi).second(s).utc_offset_hour(0).utc_offset_minute(0));
    }
    m
}

type Civil = (i64, u8, u8, u8, u8, u8);

/// `YYYY-MM-DD[THH:MM[:SS]]…` → civil time.
pub(crate) fn parse_iso(s: &str) -> Option<Civil> {
    let s = s.trim();
    let n = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let h = n(11, 13).unwrap_or(0).clamp(0, 23);
    let mi = n(14, 16).unwrap_or(0).clamp(0, 59);
    let sec = n(17, 19).unwrap_or(0).clamp(0, 59);
    Some((y, mo as u8, d as u8, h as u8, mi as u8, sec as u8))
}
