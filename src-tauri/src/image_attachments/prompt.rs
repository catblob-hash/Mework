//! Claude Code 2.1.284's prompt-image pipeline, for pictures a user attaches.
//!
//! Claude Code runs every pasted, dragged or SDK-supplied image through two
//! stages, and so does this module:
//!
//! 1. [`resize_and_compress`] (Claude Code's `IAe`) brings the image within
//!    2000 × 2000 px and 3.75 MB (5 MB of base64). An image already within both
//!    passes through. One small enough but too heavy is tried as a palette PNG
//!    (PNG sources only) and then as JPEG at quality 80, 60, 40 and 20. One too
//!    large is scaled to fit, keeping its format if it is a JPEG or WebP and
//!    becoming a PNG otherwise; if that is still too heavy it goes down the same
//!    palette and JPEG ladder, and the last resort is a JPEG at quality 20, at
//!    most 1000 px wide.
//! 2. [`fit_byte_budget`] (`t_`) then brings anything over 500 KB (512 000
//!    bytes) under it with a JPEG quality search: quality 90 first unless the
//!    image is already a JPEG, then five steps of binary search, keeping the
//!    highest quality that fits, or else the smallest result tried.
//!
//! The thresholds, ladders and output formats are Claude Code's. What differs
//! is what every stored sidecar must be — EXIF orientation applied, metadata
//! stripped — so "pass through" sends the picture's canonical form (a JPEG's
//! own scan data without its metadata segments; anything else, its pixels
//! losslessly re-encoded) rather than the file's own bytes. A WebP that must
//! be re-encoded stays lossless (there is no lossy WebP encoder here).
//!
//! The one rule that is Mework's own: a picture with any transparency never
//! becomes a JPEG, which would flatten it. Where Claude Code would reach for
//! JPEG, it is compressed only in ways that keep its alpha channel — a palette
//! PNG with per-entry alpha — and the last resort is that palette PNG at most
//! 1000 px wide.

use std::{borrow::Cow, collections::HashMap, collections::VecDeque};

use super::{
    decode_supported_image_within, encode_canonical_png_with_limit,
    encode_canonical_webp_with_limit, encode_png_with, jpeg_exif_orientation, sniff_mime,
    CanonicalPixels, LimitedImageWriter, STORED_LIMITS, UPLOAD_LIMITS,
};

/// JPEG qualities `IAe` tries, in order, once an image is too heavy.
const JPEG_LADDER: [u8; 4] = [80, 60, 40, 20];
/// What a JPEG re-encode uses when nothing names a quality (sharp's default).
const DEFAULT_JPEG_QUALITY: u8 = 80;
const LAST_RESORT_WIDTH: u32 = 1_000;
const LAST_RESORT_QUALITY: u8 = 20;
const QUALITY_SEARCH_STEPS: usize = 5;
/// NeuQuant's recommended trade-off between palette quality and speed.
const NEUQUANT_SAMPLE_FACTOR: i32 = 10;

/// Claude Code's image limits (`dP`, and the byte budget `cPe`).
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    pub(super) max_width: u32,
    pub(super) max_height: u32,
    /// `maxBase64Size * 3 / 4`: the raw size whose base64 fits the API limit.
    pub(super) target_raw_bytes: usize,
    pub(super) byte_budget: usize,
}

impl Limits {
    pub(super) const CLAUDE_CODE: Self = Self {
        max_width: 2_000,
        max_height: 2_000,
        target_raw_bytes: 5_242_880 / 4 * 3,
        byte_budget: 512_000,
    };
}

/// An encoded image and the dimensions it displays at.
#[derive(Debug)]
pub(super) struct Processed {
    pub(super) bytes: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Format {
    fn sniff(bytes: &[u8]) -> Option<Self> {
        match sniff_mime(bytes)? {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }
}

/// Both stages: what a prompt image is sent as.
pub(super) fn process(bytes: &[u8], limits: Limits) -> Result<Processed, String> {
    let resized = resize_and_compress(bytes, limits)?;
    Ok(fit_byte_budget(resized, limits.byte_budget))
}

/// Claude Code's `IAe`.
fn resize_and_compress(bytes: &[u8], limits: Limits) -> Result<Processed, String> {
    if bytes.is_empty() {
        return Err("Image file is empty (0 bytes)".into());
    }
    let format = Format::sniff(bytes).ok_or_else(|| {
        "Unsupported image format; only PNG, JPEG, WebP, and static GIF are supported".to_owned()
    })?;
    let decoded = decode_supported_image_within(bytes, UPLOAD_LIMITS)?;
    let (width, height) = (decoded.width, decoded.height);
    let mut rgba = decoded.rgba;
    clear_transparent(&mut rgba);
    let transparent = has_transparency(&rgba);
    let target = limits.target_raw_bytes;
    let fits = width <= limits.max_width && height <= limits.max_height;

    if bytes.len() <= target && fits {
        if let Some(image) = passthrough(bytes, format, width, height, &rgba, target)? {
            return Ok(image);
        }
    }
    if fits {
        if let Some(image) = compress(format, transparent, width, height, &rgba, target)? {
            return Ok(image);
        }
    }

    let (target_width, target_height) = fit_within(width, height, limits);
    let resized = resize(&rgba, width, height, target_width, target_height);
    let first = match format {
        Format::Jpeg => Some(encode_jpeg(
            target_width,
            target_height,
            &resized,
            DEFAULT_JPEG_QUALITY,
        )?),
        Format::Webp => encode_webp(target_width, target_height, &resized, target)?,
        Format::Png | Format::Gif => encode_png(
            target_width,
            target_height,
            &resized,
            target,
            png::Compression::Default,
        )?,
    };
    if let Some(image) = first.filter(|image| image.bytes.len() <= target) {
        return Ok(image);
    }
    if let Some(image) = compress(
        format,
        transparent,
        target_width,
        target_height,
        &resized,
        target,
    )? {
        return Ok(image);
    }

    let last_width = target_width.min(LAST_RESORT_WIDTH);
    let last_height = scaled(target_height, last_width, target_width.max(1));
    let last = resize(&rgba, width, height, last_width, last_height);
    if transparent {
        // At most 1000 × 2000 one-byte indices: always within the target.
        return encode_palette_png(last_width, last_height, &last, usize::MAX)?
            .ok_or_else(|| "Could not generate palette PNG".to_owned());
    }
    encode_jpeg(last_width, last_height, &last, LAST_RESORT_QUALITY)
}

/// Claude Code's `t_` byte budget, applied to what `IAe` returned. A search
/// that cannot run leaves the image as it was, unbudgeted, as there.
fn fit_byte_budget(image: Processed, budget: usize) -> Processed {
    if image.bytes.len() <= budget {
        return image;
    }
    match shrink_within(&image, budget) {
        Ok(Some(found)) => found,
        _ => image,
    }
}

/// The byte budget's search: Claude Code's JPEG quality search for an opaque
/// picture, and for one with any transparency, the palette PNG instead.
fn shrink_within(image: &Processed, budget: usize) -> Result<Option<Processed>, String> {
    let decoded = decode_supported_image_within(&image.bytes, STORED_LIMITS)?;
    if has_transparency(&decoded.rgba) {
        return Ok(
            encode_palette_png(image.width, image.height, &decoded.rgba, usize::MAX)?
                .filter(|palette| palette.bytes.len() < image.bytes.len()),
        );
    }
    search_jpeg_quality(image, &decoded.rgba, budget)
}

/// Claude Code's `oOr`.
fn search_jpeg_quality(
    image: &Processed,
    rgba: &[u8],
    budget: usize,
) -> Result<Option<Processed>, String> {
    let encode = |quality| encode_jpeg(image.width, image.height, rgba, quality);
    // `None` stands for the input itself, which is what is left when nothing
    // tried comes out smaller.
    let mut smallest: Option<Processed> = None;
    let mut smallest_length = image.bytes.len();
    let mut high = 90_u8;
    if sniff_mime(&image.bytes) != Some("image/jpeg") {
        let first = encode(90)?;
        if first.bytes.len() <= budget {
            return Ok(Some(first));
        }
        if first.bytes.len() < smallest_length {
            smallest_length = first.bytes.len();
            smallest = Some(first);
        }
        high = 89;
    }
    let mut low = 1_u8;
    let mut best = None;
    for _ in 0..QUALITY_SEARCH_STEPS {
        let quality = ((u16::from(low) + u16::from(high)) / 2) as u8;
        let candidate = encode(quality)?;
        let length = candidate.bytes.len();
        if length <= budget {
            low = quality + 1;
            best = Some(candidate);
        } else {
            high = quality - 1;
            if length < smallest_length {
                smallest_length = length;
                smallest = Some(candidate);
            }
        }
        if low > high {
            break;
        }
    }
    Ok(best.or(smallest))
}

/// An image already within both limits, as Mework may store it.
///
/// A JPEG facing the right way keeps its own scan data, so it is sent as
/// Claude Code would send it, less the metadata. Anything else is its pixels,
/// losslessly; `None` when that form is over `target`.
fn passthrough(
    bytes: &[u8],
    format: Format,
    width: u32,
    height: u32,
    rgba: &[u8],
    target: usize,
) -> Result<Option<Processed>, String> {
    if format == Format::Jpeg && jpeg_exif_orientation(bytes)? == 1 {
        let stripped = strip_jpeg_metadata(bytes)?;
        let decodes = decode_supported_image_within(&stripped, STORED_LIMITS)
            .is_ok_and(|decoded| (decoded.width, decoded.height) == (width, height));
        if decodes {
            return Ok(Some(Processed {
                bytes: stripped,
                width,
                height,
            }));
        }
    }
    let pixels = CanonicalPixels::from_rgba(rgba.to_vec());
    if let Some(bytes) = encode_canonical_png_with_limit(width, height, &pixels, target)? {
        return Ok(Some(Processed {
            bytes,
            width,
            height,
        }));
    }
    Ok(
        encode_canonical_webp_with_limit(width, height, &pixels, target)?.map(|bytes| Processed {
            bytes,
            width,
            height,
        }),
    )
}

/// The palette PNG (PNG sources, and any picture with transparency) and then,
/// for an opaque picture only, the JPEG ladder; the first to fit.
fn compress(
    format: Format,
    transparent: bool,
    width: u32,
    height: u32,
    rgba: &[u8],
    target: usize,
) -> Result<Option<Processed>, String> {
    if format == Format::Png || transparent {
        if let Some(image) = encode_palette_png(width, height, rgba, target)? {
            return Ok(Some(image));
        }
    }
    if transparent {
        return Ok(None);
    }
    for quality in JPEG_LADDER {
        let image = encode_jpeg(width, height, rgba, quality)?;
        if image.bytes.len() <= target {
            return Ok(Some(image));
        }
    }
    Ok(None)
}

/// Claude Code's target dimensions: width first, then height, each rounded.
fn fit_within(width: u32, height: u32, limits: Limits) -> (u32, u32) {
    let (mut width, mut height) = (width, height);
    if width > limits.max_width {
        height = scaled(height, limits.max_width, width);
        width = limits.max_width;
    }
    if height > limits.max_height {
        width = scaled(width, limits.max_height, height);
        height = limits.max_height;
    }
    (width.max(1), height.max(1))
}

/// `round(value * numerator / denominator)`, halves up as `Math.round` does,
/// and never zero.
fn scaled(value: u32, numerator: u32, denominator: u32) -> u32 {
    let product = u64::from(value) * u64::from(numerator);
    let denominator = u64::from(denominator.max(1));
    let rounded = (product * 2 + denominator) / (denominator * 2);
    u32::try_from(rounded).unwrap_or(u32::MAX).max(1)
}

fn clear_transparent(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            pixel[..3].fill(0);
        }
    }
}

/// JPEG quality of a timeline chip's thumbnail: the chip is small and the
/// full picture is a click away.
const THUMBNAIL_JPEG_QUALITY: u8 = 82;

/// A picture for the timeline chip: scaled down, never up, until it just
/// covers `cover_width × cover_height` (the chip draws with `object-fit:
/// cover`), then encoded the way prompt images are — a JPEG, or a PNG when
/// any pixel is transparent. `None` when the picture is no larger than that:
/// the chip draws the original.
pub(super) fn thumbnail(
    width: u32,
    height: u32,
    rgba: &[u8],
    cover_width: u32,
    cover_height: u32,
) -> Result<Option<Processed>, String> {
    if width == 0 || height == 0 {
        return Err("An empty image has no thumbnail".into());
    }
    let scale = (f64::from(cover_width) / f64::from(width))
        .max(f64::from(cover_height) / f64::from(height));
    if scale >= 1.0 {
        return Ok(None);
    }
    let target_width = ((f64::from(width) * scale).round() as u32).clamp(1, width);
    let target_height = ((f64::from(height) * scale).round() as u32).clamp(1, height);
    let pixels = resize(rgba, width, height, target_width, target_height);
    if has_transparency(&pixels) {
        encode_png(
            target_width,
            target_height,
            &pixels,
            usize::MAX,
            png::Compression::Fast,
        )?
        .ok_or_else(|| "The thumbnail could not be encoded".to_owned())
        .map(Some)
    } else {
        encode_jpeg(target_width, target_height, &pixels, THUMBNAIL_JPEG_QUALITY).map(Some)
    }
}

/// Whether any pixel is less than opaque: such a picture is never a JPEG.
fn has_transparency(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|pixel| pixel[3] != u8::MAX)
}

pub(super) fn resize(
    rgba: &[u8],
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Cow<'_, [u8]> {
    if (width, height) == (target_width, target_height) {
        Cow::Borrowed(rgba)
    } else {
        Cow::Owned(lanczos3_resize(
            rgba,
            width as usize,
            height as usize,
            target_width as usize,
            target_height as usize,
        ))
    }
}

/// Lanczos-3 downscaling (sharp's default kernel) in premultiplied alpha, so
/// a transparent pixel's colour never bleeds into its neighbours.
///
/// Rows are filtered horizontally once each, as the vertical window reaches
/// them, and dropped once it has passed: only a window's worth of filtered rows
/// is ever held, not a second copy of the image.
fn lanczos3_resize(
    rgba: &[u8],
    width: usize,
    height: usize,
    target_width: usize,
    target_height: usize,
) -> Vec<u8> {
    let columns = lanczos3_taps(width, target_width);
    let rows = lanczos3_taps(height, target_height);
    let mut filtered: VecDeque<(usize, Vec<f32>)> = VecDeque::new();
    let mut premultiplied = vec![0_f32; width * 4];
    let mut accumulated = vec![0_f32; target_width * 4];
    let mut output = Vec::with_capacity(target_width * target_height * 4);
    for (first, weights) in &rows {
        while filtered.front().is_some_and(|(row, _)| row < first) {
            filtered.pop_front();
        }
        let next = filtered.back().map_or(*first, |(row, _)| row + 1);
        for row in next..first + weights.len() {
            let source = &rgba[row * width * 4..(row + 1) * width * 4];
            filtered.push_back((row, filter_row(source, &columns, &mut premultiplied)));
        }
        debug_assert_eq!(filtered.front().map(|(row, _)| *row), Some(*first));
        accumulated.fill(0.0);
        for (weight, (_, row)) in weights.iter().zip(&filtered) {
            for (sum, value) in accumulated.iter_mut().zip(row) {
                *sum += weight * value;
            }
        }
        for pixel in accumulated.chunks_exact(4) {
            let alpha = channel(pixel[3]);
            if alpha == 0 {
                output.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // `alpha` rounded to at least 1, so the unrounded value is at least 0.5.
            let unpremultiply = 255.0 / pixel[3].min(255.0);
            output.extend_from_slice(&[
                channel(pixel[0] * unpremultiply),
                channel(pixel[1] * unpremultiply),
                channel(pixel[2] * unpremultiply),
                alpha,
            ]);
        }
    }
    output
}

fn filter_row(row: &[u8], columns: &[(usize, Vec<f32>)], premultiplied: &mut [f32]) -> Vec<f32> {
    for (target, pixel) in premultiplied.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
        let alpha = f32::from(pixel[3]) / 255.0;
        target[0] = f32::from(pixel[0]) * alpha;
        target[1] = f32::from(pixel[1]) * alpha;
        target[2] = f32::from(pixel[2]) * alpha;
        target[3] = f32::from(pixel[3]);
    }
    let mut filtered = vec![0_f32; columns.len() * 4];
    for ((first, weights), out) in columns.iter().zip(filtered.chunks_exact_mut(4)) {
        for (offset, weight) in weights.iter().enumerate() {
            let source = &premultiplied[(first + offset) * 4..][..4];
            for (sum, value) in out.iter_mut().zip(source) {
                *sum += weight * value;
            }
        }
    }
    filtered
}

/// For each target index, the first source index it reads and the normalized
/// weights of the source indices from there on.
fn lanczos3_taps(source: usize, target: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = source as f64 / target as f64;
    // Downscaling widens the kernel over the source, which is what keeps it
    // from aliasing.
    let stretch = scale.max(1.0);
    let support = 3.0 * stretch;
    (0..target)
        .map(|index| {
            let center = (index as f64 + 0.5) * scale;
            let first = (center - support).floor().max(0.0) as usize;
            let last = ((center + support).ceil() as usize)
                .min(source)
                .max(first + 1);
            let weights = (first..last)
                .map(|position| lanczos3((position as f64 + 0.5 - center) / stretch))
                .collect::<Vec<_>>();
            let sum = weights.iter().sum::<f64>();
            if sum.abs() < f64::EPSILON {
                let nearest = (center as usize).clamp(first, last - 1);
                let mut weights = vec![0_f32; last - first];
                weights[nearest - first] = 1.0;
                return (first, weights);
            }
            (
                first,
                weights.iter().map(|weight| (weight / sum) as f32).collect(),
            )
        })
        .collect()
}

fn lanczos3(x: f64) -> f64 {
    if x.abs() < f64::EPSILON {
        return 1.0;
    }
    if x.abs() >= 3.0 {
        return 0.0;
    }
    let angle = std::f64::consts::PI * x;
    3.0 * angle.sin() * (angle / 3.0).sin() / (angle * angle)
}

fn channel(value: f32) -> u8 {
    (value.clamp(0.0, 255.0) + 0.5) as u8
}

/// A baseline JPEG of `rgba`, grayscale when every pixel is. Only opaque
/// pictures are made JPEGs; should a pixel that is not reach here, JPEG has no
/// alpha, so it is composited onto white.
fn encode_jpeg(width: u32, height: u32, rgba: &[u8], quality: u8) -> Result<Processed, String> {
    let too_large = || format!("{width} × {height} px is too large for a JPEG");
    let jpeg_width = u16::try_from(width).map_err(|_| too_large())?;
    let jpeg_height = u16::try_from(height).map_err(|_| too_large())?;
    let grayscale = rgba
        .chunks_exact(4)
        .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2]);
    let (samples, color) = if grayscale {
        (
            rgba.chunks_exact(4)
                .map(|pixel| over_white(pixel[0], pixel[3]))
                .collect::<Vec<_>>(),
            jpeg_encoder::ColorType::Luma,
        )
    } else {
        (
            rgba.chunks_exact(4)
                .flat_map(|pixel| {
                    [
                        over_white(pixel[0], pixel[3]),
                        over_white(pixel[1], pixel[3]),
                        over_white(pixel[2], pixel[3]),
                    ]
                })
                .collect::<Vec<_>>(),
            jpeg_encoder::ColorType::Rgb,
        )
    };
    let mut bytes = Vec::new();
    // The standard Huffman tables, not optimized ones: jpeg-encoder optimizes
    // only by writing a scan per component, which zune-jpeg mislays or rejects
    // once a subsampled picture's luma is an odd number of blocks across or
    // down, and which other decoders, providers' among them, misread as well.
    // One interleaved scan is what libjpeg (sharp, Claude Code) writes.
    let encoder = jpeg_encoder::Encoder::new(&mut bytes, quality);
    encoder
        .encode(&samples, jpeg_width, jpeg_height, color)
        .map_err(|error| format!("Could not encode JPEG: {error}"))?;
    Ok(Processed {
        bytes,
        width,
        height,
    })
}

pub(super) fn over_white(channel: u8, alpha: u8) -> u8 {
    let (channel, alpha) = (u32::from(channel), u32::from(alpha));
    ((channel * alpha + 255 * (255 - alpha) + 127) / 255) as u8
}

fn encode_png(
    width: u32,
    height: u32,
    rgba: &[u8],
    max_bytes: usize,
    compression: png::Compression,
) -> Result<Option<Processed>, String> {
    let pixels = CanonicalPixels::from_rgba(rgba.to_vec());
    Ok(
        encode_png_with(width, height, &pixels, max_bytes, compression)?.map(|bytes| Processed {
            bytes,
            width,
            height,
        }),
    )
}

fn encode_webp(
    width: u32,
    height: u32,
    rgba: &[u8],
    max_bytes: usize,
) -> Result<Option<Processed>, String> {
    let pixels = CanonicalPixels::from_rgba(rgba.to_vec());
    Ok(
        encode_canonical_webp_with_limit(width, height, &pixels, max_bytes)?.map(|bytes| {
            Processed {
                bytes,
                width,
                height,
            }
        }),
    )
}

/// sharp's `png({compressionLevel: 9, palette: true})`: at most 256 colours,
/// exact when the picture has no more, quantized (NeuQuant) when it does.
/// `None` when it is over `max_bytes`.
fn encode_palette_png(
    width: u32,
    height: u32,
    rgba: &[u8],
    max_bytes: usize,
) -> Result<Option<Processed>, String> {
    let (palette, indices) = quantize(rgba);
    let mut colors = Vec::with_capacity(palette.len() * 3);
    let mut alphas = Vec::with_capacity(palette.len());
    for entry in &palette {
        colors.extend_from_slice(&entry[..3]);
        alphas.push(entry[3]);
    }
    // tRNS may stop at the last entry that is not opaque.
    let translucent = alphas.iter().rposition(|alpha| *alpha != u8::MAX);
    let mut output = LimitedImageWriter::with_limit(max_bytes);
    let result = (|| -> Result<(), png::EncodingError> {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_palette(colors);
        if let Some(last) = translucent {
            encoder.set_trns(alphas[..=last].to_vec());
        }
        encoder.set_compression(png::Compression::Best);
        encoder.set_filter(png::FilterType::NoFilter);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&indices)?;
        writer.finish()
    })();
    match result {
        Ok(()) => Ok(Some(Processed {
            bytes: output.bytes,
            width,
            height,
        })),
        Err(_) if output.exceeded => Ok(None),
        Err(error) => Err(format!("Could not generate palette PNG: {error}")),
    }
}

/// At most 256 colours for `rgba`.
///
/// NeuQuant learns alpha as a fourth colour channel, so the entries it settles
/// on can be a little transparent where every pixel they stand for is opaque.
/// Two things keep the alpha channel what it was: fully transparent pixels get
/// an entry of their own rather than a share of the network, and every entry
/// is then made the mean of the pixels mapped to it, which is exactly opaque
/// when they all are. Entries no pixel maps to are dropped.
fn quantize(rgba: &[u8]) -> (Vec<[u8; 4]>, Vec<u8>) {
    if let Some(exact) = exact_palette(rgba) {
        return exact;
    }
    let visible = rgba
        .chunks_exact(4)
        .filter(|pixel| pixel[3] != 0)
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let clear = visible.len() < rgba.len();
    let first = usize::from(clear);
    let quantizer = color_quant::NeuQuant::new(NEUQUANT_SAMPLE_FACTOR, 256 - first, &visible);
    let mut indices = rgba
        .chunks_exact(4)
        .map(|pixel| {
            if pixel[3] == 0 {
                0
            } else {
                (quantizer.index_of(pixel) + first) as u8
            }
        })
        .collect::<Vec<_>>();
    // Per entry: the sums of its pixels' four channels, and how many there are.
    let mut sums = [[0_u64; 5]; 256];
    for (pixel, index) in rgba.chunks_exact(4).zip(&indices) {
        let sum = &mut sums[usize::from(*index)];
        for (total, value) in sum.iter_mut().zip(pixel) {
            *total += u64::from(*value);
        }
        sum[4] += 1;
    }
    let mut palette = Vec::with_capacity(256);
    let mut renumbered = [0_u8; 256];
    for (index, sum) in sums.iter().enumerate() {
        let count = sum[4];
        if count == 0 {
            continue;
        }
        renumbered[index] = palette.len() as u8;
        let mut entry = [0_u8; 4];
        for (value, total) in entry.iter_mut().zip(sum) {
            *value = ((total + count / 2) / count) as u8;
        }
        palette.push(entry);
    }
    for index in &mut indices {
        *index = renumbered[usize::from(*index)];
    }
    (palette, indices)
}

fn exact_palette(rgba: &[u8]) -> Option<(Vec<[u8; 4]>, Vec<u8>)> {
    let mut lookup = HashMap::new();
    let mut palette = Vec::new();
    let mut indices = Vec::with_capacity(rgba.len() / 4);
    for pixel in rgba.chunks_exact(4) {
        let color = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let index = match lookup.get(&color) {
            Some(index) => *index,
            None => {
                if palette.len() == 256 {
                    return None;
                }
                let index = palette.len() as u8;
                lookup.insert(color, index);
                palette.push(color);
                index
            }
        };
        indices.push(index);
    }
    Some((palette, indices))
}

/// A JPEG's own coding data with everything a decoder does not need removed:
/// EXIF, XMP, ICC profiles, comments, JFIF thumbnails, fill bytes and whatever
/// follows EOI. The frame, tables and scans are copied byte for byte.
pub(super) fn strip_jpeg_metadata(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err("Invalid JPEG SOI marker".into());
    }
    let mut output = Vec::with_capacity(bytes.len());
    output.extend_from_slice(&[0xff, 0xd8]);
    let mut cursor = 2_usize;
    loop {
        if bytes.get(cursor) != Some(&0xff) {
            return Err("Invalid JPEG marker boundary".into());
        }
        while bytes.get(cursor) == Some(&0xff) {
            cursor += 1;
        }
        let marker = *bytes
            .get(cursor)
            .ok_or_else(|| "Truncated JPEG marker".to_owned())?;
        cursor += 1;
        if marker == 0xd9 {
            output.extend_from_slice(&[0xff, 0xd9]);
            return Ok(output);
        }
        if matches!(marker, 0x00 | 0x01 | 0xd0..=0xd8) {
            return Err("JPEG has a stray marker outside its scan data".into());
        }
        let length_end = cursor
            .checked_add(2)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "Truncated JPEG segment length".to_owned())?;
        let length = usize::from(u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]));
        if length < 2 {
            return Err("JPEG segment length is less than 2".into());
        }
        let segment_end = cursor
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "Truncated JPEG segment data".to_owned())?;
        if let Some(kept) = kept_segment(marker, &bytes[length_end..segment_end]) {
            let kept_length =
                u16::try_from(kept.len() + 2).map_err(|_| "JPEG segment is too long".to_owned())?;
            output.extend_from_slice(&[0xff, marker]);
            output.extend_from_slice(&kept_length.to_be_bytes());
            output.extend_from_slice(&kept);
        }
        cursor = segment_end;
        if marker == 0xda {
            let scan_end = entropy_coded_end(bytes, cursor)?;
            output.extend_from_slice(&bytes[cursor..scan_end]);
            cursor = scan_end;
        }
    }
}

/// What of one segment the canonical JPEG keeps: frame and scan headers and
/// coding tables whole, JFIF's header without its thumbnail, and Adobe's colour
/// transform flag, which changes how the scan decodes.
fn kept_segment(marker: u8, payload: &[u8]) -> Option<Cow<'_, [u8]>> {
    match marker {
        // SOF0–SOF15 (except the reserved JPG), DHT, DAC, DQT, DRI and SOS.
        0xc0..=0xc7 | 0xc9..=0xcf | 0xda | 0xdb | 0xdd => Some(Cow::Borrowed(payload)),
        0xe0 if payload.starts_with(b"JFIF\0") && payload.len() >= 14 => {
            let mut header = payload[..14].to_vec();
            header[12] = 0;
            header[13] = 0;
            Some(Cow::Owned(header))
        }
        0xee if payload.starts_with(b"Adobe") && payload.len() >= 12 => {
            Some(Cow::Borrowed(&payload[..12]))
        }
        _ => None,
    }
}

/// Where a scan's entropy-coded data ends: the first marker that is neither a
/// stuffed `FF 00` nor a restart marker.
fn entropy_coded_end(bytes: &[u8], start: usize) -> Result<usize, String> {
    let mut cursor = start;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == 0xff && !matches!(bytes[cursor + 1], 0x00 | 0xd0..=0xd7) {
            return Ok(cursor);
        }
        cursor += if bytes[cursor] == 0xff { 2 } else { 1 };
    }
    Err("Truncated JPEG scan data".into())
}

/// A stored JPEG is canonical when stripping would leave it as it is.
pub(super) fn validate_canonical_jpeg(bytes: &[u8]) -> Result<(), String> {
    if strip_jpeg_metadata(bytes)? != bytes {
        return Err("Canonical JPEG must not contain metadata, fill bytes or trailing data".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{decode_supported_image, validate_canonical_png_container};
    use super::*;

    /// Room enough that only the dimensions decide.
    fn dimensions_only(max: u32) -> Limits {
        Limits {
            max_width: max,
            max_height: max,
            target_raw_bytes: usize::MAX / 2,
            byte_budget: usize::MAX / 2,
        }
    }

    fn pixels(width: u32, height: u32, color: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| color(x, y))
            .collect()
    }

    /// Deterministic noise: nothing compresses it.
    fn noise(width: u32, height: u32) -> Vec<u8> {
        let mut state = 0x2545_f491_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        };
        (0..width * height)
            .flat_map(|_| [next(), next(), next(), 255])
            .collect()
    }

    /// A smooth picture with film grain: heavy as a PNG, light as a JPEG.
    fn grainy(width: u32, height: u32) -> Vec<u8> {
        let grain = noise(width, height);
        pixels(width, height, |x, y| {
            let offset = ((y * width + x) * 4) as usize;
            let base = (x * 3 + y * 2) as u8;
            [
                base.wrapping_add(grain[offset] & 15),
                base.wrapping_add(grain[offset + 1] & 15),
                base / 2 + (grain[offset + 2] & 15),
                255,
            ]
        })
    }

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        pixels(width, height, |x, y| {
            [(x * 4) as u8, (y * 4) as u8, 128, 255]
        })
    }

    fn png_of(width: u32, height: u32, rgba: &[u8], comment: Option<&str>) -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut output, width, height);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            if let Some(comment) = comment {
                writer
                    .write_text_chunk(&png::text_metadata::TEXtChunk::new("Comment", comment))
                    .unwrap();
            }
            writer.write_image_data(rgba).unwrap();
            writer.finish().unwrap();
        }
        output
    }

    fn jpeg_of(width: u32, height: u32, rgba: &[u8], quality: u8) -> Vec<u8> {
        encode_jpeg(width, height, rgba, quality).unwrap().bytes
    }

    fn webp_of(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        image_webp::WebPEncoder::new(&mut output)
            .encode(rgba, width, height, image_webp::ColorType::Rgba8)
            .unwrap();
        output
    }

    /// `jpeg` with an EXIF segment (orientation and a private string), a
    /// comment, and a trailer after EOI.
    fn with_metadata(jpeg: &[u8], orientation: u8) -> Vec<u8> {
        let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        exif.extend_from_slice(b"PRIVATE_GPS");
        let comment = b"PRIVATE_COMMENT";
        let mut output = jpeg[..2].to_vec();
        output.extend_from_slice(b"\xff\xe1");
        output.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
        output.extend_from_slice(&exif);
        output.extend_from_slice(b"\xff\xfe");
        output.extend_from_slice(&u16::try_from(comment.len() + 2).unwrap().to_be_bytes());
        output.extend_from_slice(comment);
        output.extend_from_slice(&jpeg[2..]);
        output.extend_from_slice(b"PRIVATE_TRAILER");
        output
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn mime(image: &Processed) -> &'static str {
        sniff_mime(&image.bytes).unwrap()
    }

    #[test]
    fn claude_codes_limits() {
        let limits = Limits::CLAUDE_CODE;
        assert_eq!((limits.max_width, limits.max_height), (2_000, 2_000));
        assert_eq!(limits.target_raw_bytes, 3_932_160);
        assert_eq!(limits.byte_budget, 512_000);
    }

    #[test]
    fn a_png_within_both_limits_passes_through_as_its_pixels_without_metadata() {
        let rgba = gradient(40, 30);
        let source = png_of(40, 30, &rgba, Some("PRIVATE_METADATA"));
        let image = process(&source, Limits::CLAUDE_CODE).unwrap();
        assert_eq!((image.width, image.height), (40, 30));
        assert_eq!(mime(&image), "image/png");
        validate_canonical_png_container(&image.bytes).unwrap();
        assert!(!contains(&image.bytes, b"PRIVATE"));
        assert_eq!(decode_supported_image(&image.bytes).unwrap().rgba, rgba);
    }

    #[test]
    fn a_jpeg_within_both_limits_keeps_its_scan_data_but_not_its_metadata() {
        let clean = jpeg_of(24, 16, &gradient(24, 16), 90);
        let source = with_metadata(&clean, 1);
        let image = process(&source, Limits::CLAUDE_CODE).unwrap();
        assert_eq!(mime(&image), "image/jpeg");
        assert_eq!(
            image.bytes, clean,
            "the encoder's own output has nothing to strip"
        );
        assert!(!contains(&image.bytes, b"PRIVATE"));
        assert!(!contains(&image.bytes, b"Exif"));
        validate_canonical_jpeg(&image.bytes).unwrap();
    }

    #[test]
    fn a_turned_jpeg_is_rotated_rather_than_passed_through() {
        let clean = jpeg_of(24, 16, &gradient(24, 16), 90);
        let image = process(&with_metadata(&clean, 6), Limits::CLAUDE_CODE).unwrap();
        assert_eq!((image.width, image.height), (16, 24));
        assert_eq!(mime(&image), "image/png");
        assert!(!contains(&image.bytes, b"PRIVATE"));
    }

    #[test]
    fn an_oversized_image_is_scaled_to_fit_in_claude_codes_output_format() {
        let limits = dimensions_only(32);
        let rgba = gradient(100, 33);
        // Width first: 33 * 32 / 100 = 10.56, rounded.
        let jpeg = process(&jpeg_of(100, 33, &rgba, 90), limits).unwrap();
        assert_eq!(
            (jpeg.width, jpeg.height, mime(&jpeg)),
            (32, 11, "image/jpeg")
        );
        let webp = process(&webp_of(100, 33, &rgba), limits).unwrap();
        assert_eq!(
            (webp.width, webp.height, mime(&webp)),
            (32, 11, "image/webp")
        );
        let png = process(&png_of(20, 80, &gradient(20, 80), None), limits).unwrap();
        assert_eq!((png.width, png.height, mime(&png)), (8, 32, "image/png"));
        let gif = vec![
            71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255, 44, 0, 0, 0, 0,
            1, 0, 1, 0, 0, 2, 2, 68, 1, 0, 59,
        ];
        assert_eq!(mime(&process(&gif, limits).unwrap()), "image/png");
    }

    #[test]
    fn inputs_beyond_the_stored_limits_are_accepted_and_scaled_down() {
        let rgba = gradient(9_000, 2);
        let source = png_of(9_000, 2, &rgba, None);
        let image = process(&source, Limits::CLAUDE_CODE).unwrap();
        assert_eq!((image.width, image.height), (2_000, 1));
    }

    #[test]
    fn a_heavy_png_within_the_dimensions_becomes_a_palette_png() {
        let rgba = noise(64, 64);
        let source = png_of(64, 64, &rgba, None);
        let limits = Limits {
            target_raw_bytes: 8_000,
            ..dimensions_only(2_000)
        };
        assert!(source.len() > limits.target_raw_bytes);
        let image = process(&source, limits).unwrap();
        assert_eq!(
            (image.width, image.height, mime(&image)),
            (64, 64, "image/png")
        );
        assert!(image.bytes.len() <= limits.target_raw_bytes);
        assert!(contains(&image.bytes, b"PLTE"));
        validate_canonical_png_container(&image.bytes).unwrap();
    }

    #[test]
    fn a_picture_of_few_colours_keeps_them_exactly_in_its_palette() {
        let rgba = pixels(16, 16, |x, y| {
            if (x + y) % 2 == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 128]
            }
        });
        let image = encode_palette_png(16, 16, &rgba, usize::MAX)
            .unwrap()
            .unwrap();
        assert!(contains(&image.bytes, b"tRNS"));
        validate_canonical_png_container(&image.bytes).unwrap();
        assert_eq!(decode_supported_image(&image.bytes).unwrap().rgba, rgba);
    }

    #[test]
    fn a_quantized_palette_leaves_opaque_pixels_opaque_and_clear_ones_clear() {
        let opaque = encode_palette_png(64, 64, &noise(64, 64), usize::MAX)
            .unwrap()
            .unwrap();
        assert!(!contains(&opaque.bytes, b"tRNS"));
        assert!(decode_supported_image(&opaque.bytes)
            .unwrap()
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel[3] == u8::MAX));

        let rgba = cut_out(64, 64, &noise(64, 64));
        let cut = encode_palette_png(64, 64, &rgba, usize::MAX)
            .unwrap()
            .unwrap();
        let decoded = decode_supported_image(&cut.bytes).unwrap().rgba;
        for (quantized, source) in decoded.chunks_exact(4).zip(rgba.chunks_exact(4)) {
            assert_eq!(quantized[3], source[3]);
        }
    }

    #[test]
    fn a_heavy_jpeg_goes_down_the_quality_ladder() {
        let source = jpeg_of(64, 64, &noise(64, 64), 100);
        let decoded = decode_supported_image(&source).unwrap();
        let at = |quality| encode_jpeg(64, 64, &decoded.rgba, quality).unwrap().bytes;
        let expected = at(60);
        assert!(at(80).len() > expected.len());
        let limits = Limits {
            target_raw_bytes: expected.len(),
            ..dimensions_only(2_000)
        };
        assert!(source.len() > limits.target_raw_bytes);
        assert_eq!(process(&source, limits).unwrap().bytes, expected);
    }

    #[test]
    fn the_last_resort_is_quality_20_at_most_1000_px_wide() {
        let hopeless = |max| Limits {
            target_raw_bytes: 10,
            ..dimensions_only(max)
        };
        let small = process(&png_of(64, 48, &noise(64, 48), None), hopeless(32)).unwrap();
        assert_eq!(
            (small.width, small.height, mime(&small)),
            (32, 24, "image/jpeg")
        );

        let wide = process(&png_of(1_200, 10, &noise(1_200, 10), None), hopeless(2_000)).unwrap();
        // 10 * 1000 / 1200 = 8.33, rounded.
        assert_eq!(
            (wide.width, wide.height, mime(&wide)),
            (1_000, 8, "image/jpeg")
        );
    }

    #[test]
    fn the_byte_budget_tries_quality_90_first_for_a_picture_that_is_not_a_jpeg() {
        let rgba = grainy(64, 64);
        let source = png_of(64, 64, &rgba, None);
        let q90 = encode_jpeg(64, 64, &rgba, 90).unwrap().bytes;
        let passed_through = process(&source, dimensions_only(2_000)).unwrap();
        assert_eq!(mime(&passed_through), "image/png");
        assert!(q90.len() < passed_through.bytes.len());
        let limits = Limits {
            byte_budget: q90.len(),
            ..dimensions_only(2_000)
        };
        assert_eq!(process(&source, limits).unwrap().bytes, q90);
    }

    #[test]
    fn the_byte_budget_searches_down_to_the_highest_quality_that_fits() {
        let rgba = grainy(64, 64);
        let source = png_of(64, 64, &rgba, None);
        let at = |quality| encode_jpeg(64, 64, &rgba, quality).unwrap().bytes.len();
        let budget = (at(30) + at(40)) / 2;
        let limits = Limits {
            byte_budget: budget,
            ..dimensions_only(2_000)
        };
        let image = process(&source, limits).unwrap();
        assert_eq!(mime(&image), "image/jpeg");
        assert!(image.bytes.len() <= budget);
        // 45 fails, 22 passes; the three steps left only climb from there.
        assert!(
            image.bytes.len() >= at(22),
            "{} < {}",
            image.bytes.len(),
            at(22)
        );

        // Nothing fits: the smallest result tried, still a JPEG.
        let nothing = process(
            &source,
            Limits {
                byte_budget: 1,
                ..limits
            },
        )
        .unwrap();
        assert_eq!(mime(&nothing), "image/jpeg");
        assert!(nothing.bytes.len() < source.len());
    }

    #[test]
    fn resizing_keeps_flat_colour_and_does_not_bleed_transparency() {
        let rgba = pixels(40, 40, |x, _| {
            if x < 20 {
                [0, 0, 0, 0]
            } else {
                [0, 0, 255, 255]
            }
        });
        let resized = lanczos3_resize(&rgba, 40, 40, 20, 20);
        // Halving widens the kernel to six source pixels a side: column 6 is
        // the last to see no opaque pixel, column 14 the first to see no other.
        for (index, pixel) in resized.chunks_exact(4).enumerate() {
            let x = index % 20;
            if x <= 6 {
                assert_eq!(pixel, [0, 0, 0, 0], "x = {x}");
            } else if x >= 14 {
                assert_eq!(pixel, [0, 0, 255, 255], "x = {x}");
            } else if pixel[3] > 0 {
                assert!(
                    pixel[0] <= 1 && pixel[1] <= 1 && pixel[2] >= 254,
                    "x = {x}: {pixel:?}"
                );
            }
        }
    }

    #[test]
    fn jpeg_composites_transparency_onto_white() {
        let transparent = pixels(8, 8, |_, _| [0, 0, 0, 0]);
        let jpeg = encode_jpeg(8, 8, &transparent, 90).unwrap();
        let decoded = decode_supported_image(&jpeg.bytes).unwrap();
        assert!(decoded.rgba.iter().all(|value| *value >= 250));
    }

    #[test]
    fn a_jpeg_is_one_interleaved_scan_whatever_its_block_counts() {
        // 9 × 5 luma blocks against 5 × 3 MCUs of 4:2:0: a scan per component
        // would lay the luma out differently from the MCUs on both axes.
        let (width, height) = (72, 40);
        let rgba = pixels(width, height, |x, y| {
            [(x * 3) as u8, (y * 5) as u8, (x + y) as u8, 255]
        });
        for quality in [20, 60, 87] {
            let jpeg = encode_jpeg(width, height, &rgba, quality).unwrap();
            let scans = jpeg.bytes.windows(2).filter(|pair| pair == b"\xff\xda").count();
            assert_eq!(scans, 1, "quality {quality}");
            let decoded = decode_supported_image(&jpeg.bytes).unwrap().rgba;
            let error = decoded
                .iter()
                .zip(&rgba)
                .map(|(decoded, source)| u64::from(decoded.abs_diff(*source)))
                .sum::<u64>()
                / rgba.len() as u64;
            assert!(error <= 4, "quality {quality}: mean error {error}");
        }
    }

    /// `rgba` with a transparent left half, as a logo or cut-out has.
    fn cut_out(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut rgba = rgba.to_vec();
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            if (index as u32 % width) < width / 2 {
                pixel.copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        rgba
    }

    /// The cut-out survives: transparent on the left, opaque on the right. The
    /// resize kernel may soften the edge a few pixels either side of it, and an
    /// opaque pixel that shares its palette entry with a softened one takes a
    /// little of that entry's mean alpha.
    fn keeps_alpha(image: &Processed) {
        assert_ne!(mime(image), "image/jpeg");
        let decoded = decode_supported_image(&image.bytes).unwrap();
        let width = image.width as usize;
        for (index, pixel) in decoded.rgba.chunks_exact(4).enumerate() {
            let x = index % width;
            if x + 4 < width / 2 {
                assert_eq!(pixel[3], 0, "x = {x}: {pixel:?}");
            } else if x > width / 2 + 4 {
                assert!(pixel[3] >= 250, "x = {x}: {pixel:?}");
            }
        }
    }

    #[test]
    fn a_heavy_transparent_picture_becomes_a_palette_png_rather_than_a_jpeg() {
        let rgba = cut_out(64, 64, &noise(64, 64));
        let palette = encode_palette_png(64, 64, &rgba, usize::MAX)
            .unwrap()
            .unwrap();
        let limits = Limits {
            target_raw_bytes: palette.bytes.len(),
            ..dimensions_only(2_000)
        };
        // A WebP source never tried a palette before it tried JPEG.
        for source in [png_of(64, 64, &rgba, None), webp_of(64, 64, &rgba)] {
            assert!(source.len() > limits.target_raw_bytes);
            let image = process(&source, limits).unwrap();
            assert_eq!((image.width, image.height), (64, 64));
            assert!(contains(&image.bytes, b"PLTE") && contains(&image.bytes, b"tRNS"));
            validate_canonical_png_container(&image.bytes).unwrap();
            keeps_alpha(&image);
        }
    }

    #[test]
    fn a_transparent_pictures_last_resort_is_a_palette_png_at_most_1000_px_wide() {
        let hopeless = Limits {
            target_raw_bytes: 10,
            ..dimensions_only(2_000)
        };
        let source = png_of(1_200, 12, &cut_out(1_200, 12, &noise(1_200, 12)), None);
        let image = process(&source, hopeless).unwrap();
        assert_eq!(
            (image.width, image.height, mime(&image)),
            (1_000, 10, "image/png")
        );
        assert!(contains(&image.bytes, b"tRNS"));
        keeps_alpha(&image);
    }

    #[test]
    fn the_byte_budget_keeps_a_transparent_pictures_alpha() {
        let rgba = cut_out(64, 64, &grainy(64, 64));
        let source = png_of(64, 64, &rgba, None);
        let passed_through = process(&source, dimensions_only(2_000)).unwrap();
        assert_eq!(mime(&passed_through), "image/png");
        let image = process(
            &source,
            Limits {
                byte_budget: 1,
                ..dimensions_only(2_000)
            },
        )
        .unwrap();
        assert!(image.bytes.len() < passed_through.bytes.len());
        assert!(contains(&image.bytes, b"PLTE") && contains(&image.bytes, b"tRNS"));
        keeps_alpha(&image);
    }

    #[test]
    fn a_picture_that_is_all_opaque_still_goes_down_the_jpeg_ladder() {
        // An RGBA PNG whose alpha is 255 everywhere is not transparent.
        let source = png_of(64, 64, &grainy(64, 64), None);
        let image = process(
            &source,
            Limits {
                byte_budget: 1,
                ..dimensions_only(2_000)
            },
        )
        .unwrap();
        assert_eq!(mime(&image), "image/jpeg");
    }

    #[test]
    fn only_a_jpeg_stripping_leaves_unchanged_is_canonical() {
        let clean = jpeg_of(8, 8, &gradient(8, 8), 85);
        validate_canonical_jpeg(&clean).unwrap();
        let stripped = strip_jpeg_metadata(&with_metadata(&clean, 1)).unwrap();
        assert_eq!(stripped, clean);
        assert!(validate_canonical_jpeg(&with_metadata(&clean, 1)).is_err());
        let mut trailer = clean.clone();
        trailer.extend_from_slice(b"PRIVATE");
        assert!(validate_canonical_jpeg(&trailer).is_err());
        assert!(strip_jpeg_metadata(b"\x89PNG\r\n\x1a\n").is_err());
        assert!(strip_jpeg_metadata(&clean[..clean.len() - 4]).is_err());
    }
}
