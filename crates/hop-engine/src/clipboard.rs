//! Clipboard policy: inline text, HTML, RTF, and images up to 32 MiB.
//! File lists are announced and transferred only when the other side pastes.

use hop_platform::ClipboardItem;
use hop_proto::{fits_inline, ClipAnnounce, ClipKind, MAX_INLINE_BYTES};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundClip {
    pub announce: Vec<ClipAnnounce>,
    pub inline: Vec<(ClipKind, Vec<u8>)>,
    pub files: Vec<String>,
}

pub fn prepare(items: &[ClipboardItem]) -> OutboundClip {
    let mut out = OutboundClip {
        announce: Vec::new(),
        inline: Vec::new(),
        files: Vec::new(),
    };
    for item in items {
        match item {
            ClipboardItem::Text(text) => push_inline(&mut out, ClipKind::Text, text.as_bytes()),
            ClipboardItem::Html(html) => push_inline(&mut out, ClipKind::Html, html.as_bytes()),
            ClipboardItem::Rtf(bytes) => push_inline(&mut out, ClipKind::Rtf, bytes),
            ClipboardItem::Png(bytes) => push_inline(&mut out, ClipKind::Png, bytes),
            ClipboardItem::Files(paths) => {
                let names: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                out.announce.push(ClipAnnounce {
                    kind: ClipKind::Files,
                    bytes: names.len() as u64,
                });
                out.files.extend(names);
            }
        }
    }
    out
}

fn push_inline(out: &mut OutboundClip, kind: ClipKind, bytes: &[u8]) {
    if !fits_inline(bytes.len()) {
        return;
    }
    out.announce.push(ClipAnnounce {
        kind,
        bytes: bytes.len() as u64,
    });
    out.inline.push((kind, bytes.to_vec()));
}

pub fn cap() -> usize {
    MAX_INLINE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn files_are_announced_and_not_inlined() {
        let prepared = prepare(&[
            ClipboardItem::Text("hello".into()),
            ClipboardItem::Files(vec![PathBuf::from("a.txt"), PathBuf::from("b")]),
        ]);
        assert_eq!(prepared.inline.len(), 1);
        assert_eq!(prepared.files, vec!["a.txt".to_string(), "b".to_string()]);
        assert!(prepared
            .announce
            .iter()
            .any(|item| item.kind == ClipKind::Files));
    }

    #[test]
    fn oversized_images_are_dropped() {
        let big = vec![0u8; MAX_INLINE_BYTES + 1];
        let prepared = prepare(&[ClipboardItem::Png(big)]);
        assert!(prepared.inline.is_empty());
        assert!(prepared.announce.is_empty());
    }
}
