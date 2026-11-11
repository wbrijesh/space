//! Images attached to a message: normalized with macOS ImageIO before they are kept or sent.
//!
//! Every image is re-encoded: first frame only, turned upright (EXIF orientation), scaled so
//! its longest side is at most [`MAX_EDGE`] (never up), drawn into 8-bit sRGB, and written as
//! PNG when it has transparent pixels, JPEG otherwise. Nothing of the source's metadata (EXIF,
//! GPS, profiles) is carried over. The result is named by the sha256 of its bytes.

use std::ffi::c_void;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFMutableData, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, kCGColorSpaceSRGB};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationLossyCompressionQuality, kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
    kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCacheImmediately,
    kCGImageSourceThumbnailMaxPixelSize,
};
use sha2::{Digest as _, Sha256};
use space_core::session::ImageRef;

/// The longest side after scaling (Anthropic's recommended size; bigger only costs tokens).
pub const MAX_EDGE: u32 = 1568;
pub const MAX_SOURCE_BYTES: usize = 50 << 20;
/// Decoded width × height of a source image.
pub const MAX_SOURCE_PIXELS: u64 = 64_000_000;
pub const MAX_PER_MESSAGE: usize = 20;

const JPEG_QUALITY: f64 = 0.85;
const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp"];

/// A normalized image, ready to show, keep and send.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub bytes: Arc<[u8]>,
    /// `image/jpeg` or `image/png`.
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
    /// Lowercase hex sha256 of `bytes`.
    pub sha256: String,
    /// Display name: a file name only, without control characters.
    pub name: Option<String>,
}

impl Prepared {
    /// `<sha256>.jpg` or `<sha256>.png`.
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.sha256, if self.mime == "image/png" { "png" } else { "jpg" })
    }

    pub fn gpui_format(&self) -> gpui::ImageFormat {
        if self.mime == "image/png" { gpui::ImageFormat::Png } else { gpui::ImageFormat::Jpeg }
    }
}

/// Whether a path looks like an image Space can attach (by extension).
pub fn is_image_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Normalize a file's image (its size is checked before it is read).
pub fn prepare_file(path: &Path) -> Result<Prepared, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let len = fs::metadata(path).map_err(|_| "Space couldn't read this file.".to_string())?.len();
    if len > MAX_SOURCE_BYTES as u64 {
        return Err(too_large());
    }
    let data = fs::read(path).map_err(|_| "Space couldn't read this file.".to_string())?;
    prepare(&data, name)
}

/// Normalize image bytes. The error is a short sentence for the composer.
pub fn prepare(data: &[u8], name: Option<String>) -> Result<Prepared, String> {
    prepare_within(data, name, MAX_SOURCE_PIXELS)
}

fn prepare_within(data: &[u8], name: Option<String>, max_pixels: u64) -> Result<Prepared, String> {
    if data.len() > MAX_SOURCE_BYTES {
        return Err(too_large());
    }
    let unreadable = || "This file isn't an image Space can read.".to_string();
    let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(data), None) }.ok_or_else(unreadable)?;
    if unsafe { source.count() } == 0 {
        return Err(unreadable());
    }
    let (w, h) = source_size(&source).ok_or_else(unreadable)?;
    if w == 0 || h == 0 {
        return Err(unreadable());
    }
    if w * h > max_pixels {
        return Err("This image has too many pixels (over 64 million).".into());
    }
    let edge = w.max(h).min(MAX_EDGE as u64) as i64;
    let options = dictionary(&[
        (unsafe { kCGImageSourceCreateThumbnailFromImageAlways }, CFBoolean::new(true).as_ref()),
        (unsafe { kCGImageSourceCreateThumbnailWithTransform }, CFBoolean::new(true).as_ref()),
        (unsafe { kCGImageSourceShouldCacheImmediately }, CFBoolean::new(true).as_ref()),
        (unsafe { kCGImageSourceThumbnailMaxPixelSize }, CFNumber::new_i64(edge).as_ref()),
    ]);
    let image = unsafe { source.thumbnail_at_index(0, Some(options.as_opaque())) }.ok_or_else(unreadable)?;
    let (image, transparent) = to_srgb8(&image).ok_or_else(unreadable)?;
    let (width, height) = (CGImage::width(Some(&image)) as u32, CGImage::height(Some(&image)) as u32);
    let mime = if transparent { "image/png" } else { "image/jpeg" };
    let bytes: Arc<[u8]> = encode(&image, transparent).ok_or_else(unreadable)?.into();
    let sha256 = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    Ok(Prepared { bytes, mime, width, height, sha256, name: name.as_deref().and_then(display_name) })
}

/// Write the image into `dir` as [`Prepared::file_name`] unless it is already there (same name,
/// same bytes): a temp file in `dir`, synced, then renamed; owner-only.
pub fn save(p: &Prepared, dir: &Path) -> std::io::Result<ImageRef> {
    let path = write_once(dir, &p.file_name(), &p.bytes)?;
    Ok(ImageRef { path, mime: p.mime.to_string(), width: p.width, height: p.height, name: p.name.clone() })
}

/// Write `bytes` as `dir/name` unless that file is there already (attachments are named by the
/// sha256 of their bytes): a temp file in `dir`, synced, then renamed; owner-only.
pub fn write_once(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    if !path.exists() {
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
        let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

fn too_large() -> String {
    format!("This image is larger than {} MB.", MAX_SOURCE_BYTES >> 20)
}

/// The file name part, control characters removed (`None` if nothing is left).
fn display_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_string())
}

fn dictionary(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let (keys, values): (Vec<_>, Vec<_>) = pairs.iter().copied().unzip();
    CFDictionary::from_slices(&keys, &values)
}

/// The first frame's pixel size, from its properties (no decoding).
fn source_size(source: &CGImageSource) -> Option<(u64, u64)> {
    let props = unsafe { source.properties_at_index(0, None) }?;
    let props = unsafe { props.cast_unchecked::<CFString, CFType>() };
    let get = |key: &CFString| props.get(key)?.downcast_ref::<CFNumber>()?.as_i64().map(|v| v.max(0) as u64);
    Some((get(unsafe { kCGImagePropertyPixelWidth })?, get(unsafe { kCGImagePropertyPixelHeight })?))
}

/// Draw `image` into an 8-bit RGBA sRGB bitmap. Returns the new image and whether any pixel
/// is transparent (only looked for when the source has alpha at all).
fn to_srgb8(image: &CGImage) -> Option<(CFRetained<CGImage>, bool)> {
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let has_alpha = !matches!(
        CGImage::alpha_info(Some(image)),
        CGImageAlphaInfo::None | CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast
    );
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let mut pixels = vec![0u8; w * h * 4];
    let info = if has_alpha { CGImageAlphaInfo::PremultipliedLast } else { CGImageAlphaInfo::NoneSkipLast };
    let context: CFRetained<CGContext> =
        unsafe { CGBitmapContextCreate(pixels.as_mut_ptr().cast::<c_void>(), w, h, 8, w * 4, Some(&space), info.0) }?;
    let rect = CGRect::new(CGPoint::new(0., 0.), CGSize::new(w as f64, h as f64));
    CGContext::draw_image(Some(&context), rect, Some(image));
    let transparent = has_alpha && pixels.as_chunks::<4>().0.iter().any(|p| p[3] < 255);
    let out = CGBitmapContextCreateImage(Some(&context))?;
    drop(context);
    Some((out, transparent))
}

/// PNG, or JPEG at [`JPEG_QUALITY`]. Only the quality is passed, so no metadata is written.
pub(crate) fn encode(image: &CGImage, png: bool) -> Option<Vec<u8>> {
    let data = CFMutableData::new(None, 0)?;
    let kind = CFString::from_static_str(if png { "public.png" } else { "public.jpeg" });
    let dest = unsafe { CGImageDestination::with_data(&data, &kind, 1, None) }?;
    let props = (!png).then(|| dictionary(&[(unsafe { kCGImageDestinationLossyCompressionQuality }, CFNumber::new_f64(JPEG_QUALITY).as_ref())]));
    unsafe { dest.add_image(image, props.as_deref().map(|p| p.as_opaque())) };
    unsafe { dest.finalize() }.then(|| data.to_vec())
}

#[cfg(test)]
mod tests {
    use objc2_image_io::{kCGImagePropertyExifDateTimeOriginal, kCGImagePropertyExifDictionary, kCGImagePropertyGPSDictionary, kCGImagePropertyGPSLatitude, kCGImagePropertyGPSLatitudeRef, kCGImagePropertyOrientation};

    use super::*;

    /// A `w`×`h` image (`bits` per component, 8 or 16), filled by `pixel(x, y) -> [r, g, b, a]`,
    /// encoded as `uti` with extra `props` (orientation, GPS).
    fn make(w: usize, h: usize, uti: &str, bits: usize, props: Option<&CFDictionary<CFString, CFType>>, pixel: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        let bpp = bits / 8 * 4;
        let mut buf = vec![0u8; w * h * bpp];
        for y in 0..h {
            for x in 0..w {
                let [r, g, b, a] = pixel(x, y);
                let px = [r, g, b, a].map(|c| (c as u16 * a as u16 / 255) as u8); // premultiplied
                let at = (y * w + x) * bpp;
                for (i, c) in px.iter().enumerate() {
                    if bits == 16 {
                        buf[at + i * 2..at + i * 2 + 2].copy_from_slice(&(*c as u16 * 257).to_ne_bytes());
                    } else {
                        buf[at + i] = *c;
                    }
                }
            }
        }
        // 16-bit needs a byte order flag: kCGBitmapByteOrder16Little (1 << 12), the host's.
        let info = CGImageAlphaInfo::PremultipliedLast.0 | if bits == 16 { 1 << 12 } else { 0 };
        let ctx = unsafe { CGBitmapContextCreate(buf.as_mut_ptr().cast(), w, h, bits, w * bpp, Some(&space), info) }.unwrap();
        let image = CGBitmapContextCreateImage(Some(&ctx)).unwrap();
        let data = CFMutableData::new(None, 0).unwrap();
        let dest = unsafe { CGImageDestination::with_data(&data, &CFString::from_str(uti), 1, None) }.unwrap();
        unsafe { dest.add_image(&image, props.map(|p| p.as_opaque())) };
        assert!(unsafe { dest.finalize() });
        data.to_vec()
    }

    fn opaque(w: usize, h: usize) -> Vec<u8> {
        make(w, h, "public.png", 8, None, |x, y| [(x % 256) as u8, (y % 256) as u8, 90, 255])
    }

    fn props(data: &[u8]) -> CFRetained<CFDictionary> {
        let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(data), None) }.unwrap();
        unsafe { source.properties_at_index(0, None) }.unwrap()
    }

    fn has(data: &[u8], key: &CFString) -> bool {
        let props = props(data);
        unsafe { props.cast_unchecked::<CFString, CFType>() }.get(key).is_some()
    }

    /// The Exif date taken, if any.
    fn taken_at(data: &[u8]) -> Option<String> {
        let props = props(data);
        let exif = unsafe { props.cast_unchecked::<CFString, CFType>() }.get(unsafe { kCGImagePropertyExifDictionary })?;
        let exif = exif.downcast_ref::<CFDictionary>()?;
        let date = unsafe { exif.cast_unchecked::<CFString, CFType>() }.get(unsafe { kCGImagePropertyExifDateTimeOriginal })?;
        Some(date.downcast_ref::<CFString>()?.to_string())
    }

    fn decoded_size(data: &[u8]) -> (u64, u64) {
        source_size(&unsafe { CGImageSource::with_data(&CFData::from_bytes(data), None) }.unwrap()).unwrap()
    }

    #[test]
    fn large_images_are_scaled_to_jpeg() {
        let p = prepare(&opaque(4000, 1000), Some("wide.png".into())).unwrap();
        assert_eq!((p.mime, p.width, p.height), ("image/jpeg", 1568, 392));
        assert_eq!(decoded_size(&p.bytes), (1568, 392));
        assert_eq!(p.file_name(), format!("{}.jpg", p.sha256));
        assert_eq!(p.sha256.len(), 64);
        assert_eq!(p.name.as_deref(), Some("wide.png"));
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let p = prepare(&opaque(100, 50), None).unwrap();
        assert_eq!((p.width, p.height), (100, 50));
    }

    #[test]
    fn transparency_keeps_png_and_opaque_rgba_becomes_jpeg() {
        let holes = make(64, 64, "public.png", 8, None, |x, _| [200, 10, 10, if x < 32 { 0 } else { 255 }]);
        let p = prepare(&holes, None).unwrap();
        assert_eq!((p.mime, p.gpui_format()), ("image/png", gpui::ImageFormat::Png));
        assert!(p.file_name().ends_with(".png"));
        let rgba_opaque = make(64, 64, "public.png", 8, None, |_, _| [10, 200, 10, 255]);
        assert_eq!(prepare(&rgba_opaque, None).unwrap().mime, "image/jpeg");
    }

    #[test]
    fn orientation_is_applied_and_metadata_dropped() {
        let north = CFString::from_static_str("N");
        let gps = dictionary(&[
            (unsafe { kCGImagePropertyGPSLatitude }, CFNumber::new_f64(48.85).as_ref()),
            (unsafe { kCGImagePropertyGPSLatitudeRef }, north.as_ref()),
        ]);
        let taken = CFString::from_static_str("2026:11:08 10:00:00");
        let exif = dictionary(&[(unsafe { kCGImagePropertyExifDateTimeOriginal }, taken.as_ref())]);
        let extra = dictionary(&[
            (unsafe { kCGImagePropertyOrientation }, CFNumber::new_i64(6).as_ref()),
            (unsafe { kCGImagePropertyGPSDictionary }, gps.as_ref()),
            (unsafe { kCGImagePropertyExifDictionary }, exif.as_ref()),
        ]);
        let jpeg = make(200, 100, "public.jpeg", 8, Some(&extra), |_, _| [1, 2, 3, 255]);
        assert!(has(&jpeg, unsafe { kCGImagePropertyGPSDictionary }), "the source carries GPS");
        assert!(taken_at(&jpeg).is_some(), "the source carries a date taken");
        let p = prepare(&jpeg, None).unwrap();
        assert_eq!((p.width, p.height), (100, 200));
        for key in unsafe { [kCGImagePropertyOrientation, kCGImagePropertyGPSDictionary] } {
            assert!(!has(&p.bytes, key), "{key} is gone");
        }
        // The JPEG writer adds its own small {Exif} (size, colour space); nothing of the source's.
        assert_eq!(taken_at(&p.bytes), None);
    }

    #[test]
    fn sixteen_bit_becomes_eight_bit() {
        let png16 = make(80, 40, "public.png", 16, None, |x, _| [x as u8, 100, 200, 255]);
        let source = props(&png16);
        let depth = unsafe { source.cast_unchecked::<CFString, CFType>() }
            .get(unsafe { objc2_image_io::kCGImagePropertyDepth })
            .and_then(|d| d.downcast_ref::<CFNumber>()?.as_i64());
        assert_eq!(depth, Some(16));
        let p = prepare(&png16, None).unwrap();
        assert_eq!((p.mime, p.width, p.height), ("image/jpeg", 80, 40));
        let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(&p.bytes), None) }.unwrap();
        let image = unsafe { source.image_at_index(0, None) }.unwrap();
        assert_eq!(CGImage::bits_per_component(Some(&image)), 8);
    }

    #[test]
    fn gif_becomes_a_still() {
        let gif = make(30, 20, "com.compuserve.gif", 8, None, |_, _| [0, 0, 255, 255]);
        let p = prepare(&gif, Some("anim.gif".into())).unwrap();
        assert_eq!((p.mime, p.width, p.height), ("image/jpeg", 30, 20));
    }

    #[test]
    fn rejects_garbage_and_too_many_pixels() {
        assert!(prepare(b"definitely not an image", None).unwrap_err().contains("isn't an image"));
        assert!(prepare(&[], None).is_err());
        let err = prepare_within(&opaque(100, 100), None, 5_000).unwrap_err();
        assert!(err.contains("too many pixels"), "{err}");
        assert!(prepare(&vec![0u8; MAX_SOURCE_BYTES + 1], None).unwrap_err().contains("50 MB"));
    }

    #[test]
    fn same_image_same_name_saved_once() {
        let src = opaque(120, 80);
        let (a, b) = (prepare(&src, Some("a.png".into())).unwrap(), prepare(&src, Some("b.png".into())).unwrap());
        assert_eq!(a.sha256, b.sha256);
        let dir = tempfile::tempdir().unwrap();
        let attachments = dir.path().join("attachments");
        let r = save(&a, &attachments).unwrap();
        assert_eq!(r.path, attachments.join(a.file_name()));
        assert_eq!((r.mime.as_str(), r.width, r.height, r.name.as_deref()), ("image/jpeg", 120, 80, Some("a.png")));
        assert_eq!(fs::metadata(&r.path).unwrap().permissions().mode() & 0o777, 0o600);
        let written = fs::metadata(&r.path).unwrap().modified().unwrap();
        let again = save(&b, &attachments).unwrap();
        assert_eq!(again.path, r.path);
        assert_eq!(again.name.as_deref(), Some("b.png"));
        assert_eq!(fs::metadata(&r.path).unwrap().modified().unwrap(), written);
        assert_eq!(fs::read_dir(&attachments).unwrap().count(), 1, "no temp files left");
        assert_eq!(fs::read(&r.path).unwrap(), &*a.bytes);
    }

    #[test]
    fn image_paths_and_names() {
        for p in ["a.png", "b.JPG", "c.jpeg", "d.gif", "e.webp", "f.HEIC", "g.heif", "h.tiff", "i.tif", "j.bmp"] {
            assert!(is_image_path(Path::new(p)), "{p}");
        }
        for p in ["a.pdf", "b", "c.png.txt", ".png"] {
            assert!(!is_image_path(Path::new(p)), "{p}");
        }
        assert_eq!(display_name("dir/sub/sh\u{7}ot\n.png").as_deref(), Some("shot.png"));
        assert_eq!(display_name("C:\\x\\y.jpg").as_deref(), Some("y.jpg"));
        assert_eq!(display_name("\u{1b}\n"), None);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pic.png");
        fs::write(&file, opaque(10, 10)).unwrap();
        assert_eq!(prepare_file(&file).unwrap().name.as_deref(), Some("pic.png"));
        assert!(prepare_file(&dir.path().join("missing.png")).is_err());
    }
}
