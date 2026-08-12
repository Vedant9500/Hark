//! FreeDesktop thumbnail cache helpers (`~/.cache/thumbnails`).
//!
//! Shared by preview decode and drag-icon polish so MD5/URI path logic stays
//! in one place.

use gtk::gdk_pixbuf::Pixbuf;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// FreeDesktop thumbnail path (`~/.cache/thumbnails/{large,normal}/md5(uri).png`).
pub(crate) fn freedesktop_thumbnail(path: &Path) -> Option<PathBuf> {
    let digest = thumbnail_digest(path)?;
    let base = dirs::home_dir()?.join(".cache/thumbnails");
    for size in ["large", "normal", "x-large"] {
        let p = base.join(size).join(format!("{digest}.png"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Canonical FreeDesktop `file://` URI + MD5 digest used for cache names.
pub(crate) fn thumbnail_digest(path: &Path) -> Option<String> {
    let canon = path.canonicalize().ok()?;
    let uri = file_uri(&canon);
    Some(md5_hex(uri.as_bytes()))
}

fn file_uri(canon: &Path) -> String {
    // Local paths only; percent-encoding non-ascii is rare for our use.
    format!("file://{}", canon.display())
}

/// Write a FreeDesktop-style thumbnail for `source` from already-decoded pixels.
///
/// Best-effort: never fails the preview path. Uses the `large` (256) slot when
/// the image is big enough, else `normal` (128). Includes Thumb::URI / MTime
/// text chunks when gdk-pixbuf accepts them.
pub(crate) fn store_freedesktop_thumbnail(
    source: &Path,
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
    pixels: &[u8],
) -> bool {
    if width <= 0 || height <= 0 || pixels.is_empty() {
        return false;
    }
    let Ok(canon) = source.canonicalize() else {
        return false;
    };
    let uri = file_uri(&canon);
    let digest = md5_hex(uri.as_bytes());
    let Some(home) = dirs::home_dir() else {
        return false;
    };

    // Prefer large (256) when either edge is big enough; otherwise normal (128).
    let (subdir, max_edge) = if width.max(height) >= 192 {
        ("large", 256)
    } else {
        ("normal", 128)
    };
    let dir = home.join(".cache/thumbnails").join(subdir);
    if fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let dest = dir.join(format!("{digest}.png"));
    if dest.is_file() {
        return true;
    }

    let bytes = glib::Bytes::from_owned(pixels.to_vec());
    let pixbuf = Pixbuf::from_bytes(
        &bytes,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        has_alpha,
        8,
        width,
        height,
        rowstride,
    );

    let scaled = {
        let w = pixbuf.width();
        let h = pixbuf.height();
        if w > max_edge || h > max_edge {
            let scale = (max_edge as f64) / (w.max(h) as f64);
            let nw = ((w as f64) * scale).round().max(1.0) as i32;
            let nh = ((h as f64) * scale).round().max(1.0) as i32;
            pixbuf
                .scale_simple(nw, nh, gtk::gdk_pixbuf::InterpType::Bilinear)
                .unwrap_or(pixbuf)
        } else {
            pixbuf
        }
    };

    let mtime = fs::metadata(&canon)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|| "0".into());

    // Atomic-ish write via temp then rename.
    let tmp = dir.join(format!(".{digest}.hark-tmp.png"));
    let options = [
        ("tEXt::Thumb::URI", uri.as_str()),
        ("tEXt::Thumb::MTime", mtime.as_str()),
    ];
    let ok = scaled
        .savev(&tmp, "png", &options)
        .or_else(|_| scaled.savev(&tmp, "png", &[]))
        .is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
        return false;
    }
    if fs::rename(&tmp, &dest).is_err() {
        let _ = fs::remove_file(&tmp);
        return false;
    }
    true
}

fn md5_hex(message: &[u8]) -> String {
    let d = md5_bytes(message);
    let mut s = String::with_capacity(32);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Compact MD5 for FreeDesktop thumbnail names.
fn md5_bytes(message: &[u8]) -> [u8; 16] {
    fn f(x: u32, y: u32, z: u32) -> u32 {
        (x & y) | (!x & z)
    }
    fn g(x: u32, y: u32, z: u32) -> u32 {
        (x & z) | (y & !z)
    }
    fn h(x: u32, y: u32, z: u32) -> u32 {
        x ^ y ^ z
    }
    fn i(x: u32, y: u32, z: u32) -> u32 {
        y ^ (x | !z)
    }

    let mut msg = message.to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let s: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (j, item) in m.iter_mut().enumerate() {
            let o = j * 4;
            *item = u32::from_le_bytes([chunk[o], chunk[o + 1], chunk[o + 2], chunk[o + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for j in 0..64 {
            let (fval, gval) = if j < 16 {
                (f(b, c, d), j)
            } else if j < 32 {
                (g(b, c, d), (5 * j + 1) % 16)
            } else if j < 48 {
                (h(b, c, d), (3 * j + 5) % 16)
            } else {
                (i(b, c, d), (7 * j) % 16)
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(fval)
                    .wrapping_add(k[j])
                    .wrapping_add(m[gval])
                    .rotate_left(s[j]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}
