//! The switcher's model: the host's windows in the host's order, the active source, thumbnails (kind 3,
//! JPEG, decoded to RGBA and bounded), app icons (kind 4, PNG) and the installed apps (kind 5).
//!
//! The host streams one source at a time; the order is the host's (no per-Mac drag order here).

use crate::locked::{LockedMac, Why};
use sill_proto::{AppInfo, StreamSource, WindowInfo, WindowList};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A decoded picture. `id` is unique per decode, so a texture cache can key on it.
#[derive(Debug, PartialEq)]
pub struct Image {
    pub id: u64,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

static IMAGE_IDS: AtomicU64 = AtomicU64::new(1);

/// Most pixels a side of a decoded picture may have, and the most bytes of its encoding we decode.
pub const MAX_SIDE: u32 = 1024;
pub const MAX_ENCODED: usize = 1 << 20;

/// Decode a JPEG or PNG; `None` for anything that does not decode or is too large.
pub fn decode_image(bytes: &[u8]) -> Option<Image> {
    if bytes.len() > MAX_ENCODED {
        return None;
    }
    let img = image::load_from_memory(bytes).ok()?;
    if img.width() == 0 || img.height() == 0 || img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        return None;
    }
    let rgba = img.to_rgba8();
    Some(Image { id: IMAGE_IDS.fetch_add(1, Ordering::Relaxed), w: rgba.width(), h: rgba.height(), rgba: rgba.into_raw() })
}

/// Newest thumbnail per window, within a byte budget; windows no longer listed are dropped.
#[derive(Debug)]
pub struct ThumbCache {
    map: HashMap<u32, (Arc<Image>, u64)>,
    seq: u64,
    budget: usize,
}

pub const THUMB_BUDGET: usize = 8 << 20;

impl Default for ThumbCache {
    fn default() -> Self {
        ThumbCache::new(THUMB_BUDGET)
    }
}

impl ThumbCache {
    pub fn new(budget: usize) -> ThumbCache {
        ThumbCache { map: HashMap::new(), seq: 0, budget }
    }

    pub fn bytes(&self) -> usize {
        self.map.values().map(|(i, _)| i.rgba.len()).sum()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, window: u32) -> Option<Arc<Image>> {
        self.map.get(&window).map(|(i, _)| i.clone())
    }

    /// Keep the newest picture per window; evict the oldest windows' pictures while over budget (the
    /// one just inserted stays).
    pub fn insert(&mut self, window: u32, img: Arc<Image>) {
        self.seq += 1;
        self.map.insert(window, (img, self.seq));
        while self.bytes() > self.budget && self.map.len() > 1 {
            let oldest = self.map.iter().filter(|(k, _)| **k != window).min_by_key(|(_, (_, s))| *s).map(|(k, _)| *k);
            match oldest {
                Some(k) => {
                    self.map.remove(&k);
                }
                None => break,
            }
        }
    }

    pub fn retain_listed(&mut self, ids: &[u32]) {
        self.map.retain(|k, _| ids.contains(k));
    }
}

#[derive(Debug, Default)]
pub struct Switcher {
    pub mac_name: String,
    pub windows: Vec<WindowInfo>,
    pub active: Option<StreamSource>,
    pub thumbs: ThumbCache,
    icons: HashMap<String, Arc<Image>>,
    pub apps: Vec<AppInfo>,
    pub locked: Option<Why>,
    pub list: Option<WindowList>,
}

/// Icons kept at most (the host sends one per app it lists).
const MAX_ICONS: usize = 512;

impl Switcher {
    pub fn on_list(&mut self, l: &WindowList) {
        self.mac_name = l.mac_name.clone();
        self.windows = l.windows.clone();
        self.active = Some(l.active);
        self.locked = LockedMac::why(l);
        let ids: Vec<u32> = l.windows.iter().map(|w| w.id).collect();
        self.thumbs.retain_listed(&ids);
        self.list = Some(l.clone());
    }

    pub fn on_thumbnail(&mut self, window: u32, jpeg: &[u8]) -> bool {
        match decode_image(jpeg) {
            Some(i) => {
                self.thumbs.insert(window, Arc::new(i));
                true
            }
            None => false,
        }
    }

    pub fn on_icon(&mut self, bundle: &str, png: &[u8]) -> bool {
        if self.icons.len() >= MAX_ICONS && !self.icons.contains_key(bundle) {
            return false;
        }
        match decode_image(png) {
            Some(i) => {
                self.icons.insert(bundle.to_string(), Arc::new(i));
                true
            }
            None => false,
        }
    }

    pub fn icon(&self, bundle: &str) -> Option<Arc<Image>> {
        self.icons.get(bundle).cloned()
    }

    pub fn on_apps(&mut self, apps: Vec<AppInfo>) {
        self.apps = apps;
        self.apps.sort_by_key(|a| a.name.to_lowercase());
    }

    /// The installed apps whose name contains `query` (case-insensitive); all of them for an empty query.
    pub fn filtered_apps(&self, query: &str) -> Vec<&AppInfo> {
        let q = query.trim().to_lowercase();
        self.apps.iter().filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q)).collect()
    }

    /// The label a window's entry shows: its title, else its app's name.
    pub fn label(w: &WindowInfo) -> &str {
        if w.title.trim().is_empty() { &w.app_name } else { &w.title }
    }

    /// The streamed window, if the active source is one the list has.
    pub fn active_window(&self) -> Option<&WindowInfo> {
        match self.active {
            Some(StreamSource::Window(id)) => self.windows.iter().find(|w| w.id == id),
            _ => None,
        }
    }

    pub fn reset(&mut self) {
        *self = Switcher::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![];
        let e = image::codecs::png::PngEncoder::new(&mut out);
        use image::ImageEncoder;
        e.write_image(&vec![200u8; (w * h * 4) as usize], w, h, image::ExtendedColorType::Rgba8).unwrap();
        out
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![];
        let e = image::codecs::jpeg::JpegEncoder::new(&mut out);
        use image::ImageEncoder;
        e.write_image(&vec![128u8; (w * h * 3) as usize], w, h, image::ExtendedColorType::Rgb8).unwrap();
        out
    }

    fn win(id: u32, title: &str) -> WindowInfo {
        WindowInfo { id, title: title.into(), app_name: "App".into(), bundle_id: "b.app".into(), width: 800, height: 600 }
    }

    fn list(ids: &[u32], active: StreamSource) -> WindowList {
        WindowList { mac_name: "Mini".into(), windows: ids.iter().map(|&i| win(i, &format!("w{i}"))).collect(), active, launch_id: None, host_version: None, protocol: None, gestures: None, clipboard: None, key_repeat: None, locked: None, off_console: None }
    }

    #[test]
    fn decodes_jpeg_and_png_and_rejects_junk_and_huge() {
        let j = decode_image(&jpeg(64, 32)).unwrap();
        assert_eq!((j.w, j.h, j.rgba.len()), (64, 32, 64 * 32 * 4));
        assert!(decode_image(&png(16, 16)).is_some());
        assert!(decode_image(b"not an image").is_none());
        assert!(decode_image(&png(MAX_SIDE + 1, 1)).is_none());
        assert!(decode_image(&vec![0u8; MAX_ENCODED + 1]).is_none());
        assert_ne!(decode_image(&png(4, 4)).unwrap().id, decode_image(&png(4, 4)).unwrap().id);
    }

    #[test]
    fn thumbnails_keep_the_newest_per_window_and_drop_unlisted_windows() {
        let mut s = Switcher::default();
        s.on_list(&list(&[1, 2], StreamSource::Desktop));
        assert!(s.on_thumbnail(1, &jpeg(32, 32)));
        let first = s.thumbs.get(1).unwrap().id;
        assert!(s.on_thumbnail(1, &jpeg(32, 32)));
        assert_ne!(s.thumbs.get(1).unwrap().id, first, "newest replaces");
        assert_eq!(s.thumbs.len(), 1);
        s.on_thumbnail(2, &jpeg(32, 32));
        s.on_list(&list(&[2], StreamSource::Desktop));
        assert!(s.thumbs.get(1).is_none() && s.thumbs.get(2).is_some(), "a window no longer listed loses its picture");
        assert!(!s.on_thumbnail(2, b"junk"), "undecodable keeps the old one");
        assert!(s.thumbs.get(2).is_some());
    }

    #[test]
    fn thumbnail_memory_is_bounded() {
        let one = 100 * 100 * 4;
        let mut c = ThumbCache::new(one * 3 + 10);
        for id in 0..20u32 {
            let img = decode_image(&jpeg(100, 100)).unwrap();
            c.insert(id, Arc::new(img));
            assert!(c.bytes() <= one * 3 + 10, "over budget at {id}: {}", c.bytes());
        }
        assert_eq!(c.len(), 3);
        assert!(c.get(19).is_some() && c.get(18).is_some() && c.get(0).is_none(), "oldest go first");
        // A single picture over the budget stays (the newest is never evicted).
        let mut tiny = ThumbCache::new(10);
        tiny.insert(1, Arc::new(decode_image(&jpeg(10, 10)).unwrap()));
        assert_eq!(tiny.len(), 1);
    }

    #[test]
    fn apps_filter_by_name_and_sort() {
        let mut s = Switcher::default();
        s.on_apps(vec![
            AppInfo { name: "Safari".into(), bundle_id: "com.apple.Safari".into() },
            AppInfo { name: "calendar".into(), bundle_id: "com.apple.iCal".into() },
            AppInfo { name: "Notes".into(), bundle_id: "com.apple.Notes".into() },
        ]);
        assert_eq!(s.apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["calendar", "Notes", "Safari"]);
        assert_eq!(s.filtered_apps("SAF").len(), 1);
        assert_eq!(s.filtered_apps("").len(), 3);
        assert_eq!(s.filtered_apps("zzz").len(), 0);
    }

    #[test]
    fn icons_decode_and_are_bounded() {
        let mut s = Switcher::default();
        assert!(s.on_icon("b.app", &png(32, 32)));
        assert!(s.icon("b.app").is_some());
        for i in 0..MAX_ICONS + 5 {
            s.on_icon(&format!("x{i}"), &png(2, 2));
        }
        assert!(s.icons.len() <= MAX_ICONS);
    }

    #[test]
    fn the_active_window_and_labels() {
        let mut s = Switcher::default();
        s.on_list(&list(&[5], StreamSource::Window(5)));
        assert_eq!(s.active_window().unwrap().id, 5);
        assert_eq!(Switcher::label(&win(1, "  ")), "App");
        assert_eq!(Switcher::label(&win(1, "Doc")), "Doc");
        s.on_list(&list(&[5], StreamSource::Desktop));
        assert!(s.active_window().is_none());
    }
}
