//! A plain border round an exported photo: a canvas of one colour with the photo centred on it.
//! The border is even on every side, or the canvas takes a fixed shape (`ratio`, 1:1 for a square
//! post, 4:5, 9:16 …) with at least `thickness` on its narrowest sides, as a phone photo editor's
//! "add border" does. Drawn last, after output sharpening and the watermark, so the file is the
//! photo as exported with the border round it (a resized export's size is the photo's; the border
//! adds to it).

use super::encoding::srgb8_in;
use super::{DeepImage, DeepSamples, Deserialize, OutputSpace, Rgba8, Serialize};

/// `thickness`: none up to a quarter of the photo's long edge on each side.
pub const BORDER_THICKNESS_RANGE: (f32, f32) = (0.0, 0.25);

/// The widest (and, upside down, the tallest) canvas shape a `ratio` may ask for.
pub const BORDER_MAX_RATIO: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Border {
    /// sRGB, converted to the export's colour space when drawn.
    pub color: [u8; 3],
    /// The narrowest sides' width, as a fraction of the photo's long edge ([`BORDER_THICKNESS_RANGE`]).
    pub thickness: f32,
    /// The canvas's width : height (`None`: the same border on every side).
    pub ratio: Option<[u16; 2]>,
    /// The photo with its border keeps the photo's own pixel count: the photo is exported smaller
    /// to make room ([`Border::photo_size`]), so the file takes about the space the photo alone
    /// would. Off: the border adds to the photo, up to what the device can hold
    /// ([`border_too_big`]).
    pub keep_size: bool,
}

impl Default for Border {
    fn default() -> Self {
        Self { color: [255, 255, 255], thickness: 0.05, ratio: None, keep_size: false }
    }
}

impl Border {
    /// The canvas shape as width / height, when it is one LightCraft draws (both sides above
    /// zero, at most [`BORDER_MAX_RATIO`] either way).
    pub fn shape(&self) -> Option<f32> {
        let [w, h] = self.ratio?;
        let r = f32::from(w) / f32::from(h.max(1));
        (w > 0 && h > 0 && (1.0 / BORDER_MAX_RATIO..=BORDER_MAX_RATIO).contains(&r)).then_some(r)
    }

    /// The size a `w × h` photo is exported at under this border: smaller with
    /// [`Border::keep_size`], so that the canvas round it has about `w × h` pixels (the border's
    /// sizes follow the photo's, so the whole scales as one); else `w × h`.
    pub fn photo_size(&self, w: usize, h: usize) -> (usize, usize) {
        if !self.keep_size || w == 0 || h == 0 {
            return (w, h);
        }
        let (cw, ch, _, _) = self.layout(w, h);
        let k = ((w as f64 * h as f64) / (cw as f64 * ch as f64)).sqrt().min(1.0);
        (((w as f64 * k).round() as usize).max(1), ((h as f64 * k).round() as usize).max(1))
    }

    /// For a `w × h` photo: the canvas's size and where the photo's top-left corner goes on it.
    pub fn layout(&self, w: usize, h: usize) -> (usize, usize, usize, usize) {
        let (lo, hi) = BORDER_THICKNESS_RANGE;
        let t = if self.thickness.is_finite() { self.thickness.clamp(lo, hi) } else { 0.0 };
        let side = (f64::from(t) * w.max(h) as f64).round() as usize;
        let (mut cw, mut ch) = (w.saturating_add(2 * side), h.saturating_add(2 * side));
        if let Some(r) = self.shape() {
            let r = f64::from(r);
            if (cw as f64) < ch as f64 * r {
                cw = (ch as f64 * r).round() as usize;
            } else {
                ch = (cw as f64 / r).round() as usize;
            }
            // (rounding never leaves the photo without room)
            cw = cw.max(w);
            ch = ch.max(h);
        }
        (cw, ch, (cw - w) / 2, (ch - h) / 2)
    }
}

/// Why border `b` round a `w × h` photo can't be drawn with `bytes_per_pixel` a pixel within
/// `budget` bytes (0: no budget beyond the largest output there is): the canvas and the photo it
/// is drawn from are both in memory then. `None`: it can.
pub fn border_too_big(b: &Border, (w, h): (usize, usize), bytes_per_pixel: usize, budget: usize) -> Option<String> {
    let (cw, ch, _, _) = b.layout(w, h);
    if let Err(e) = lightcraft_pipeline::check_output_size(cw, ch) {
        return Some(format!("with its border {e}"));
    }
    let bytes = cw.saturating_mul(ch).saturating_add(w.saturating_mul(h)).saturating_mul(bytes_per_pixel);
    (budget > 0 && bytes > budget).then(|| {
        format!(
            "with its border the image would be {cw} × {ch} ({:.0} MP), more than this device can hold; keep the original size or choose a thinner border or another shape",
            (cw * ch) as f64 / 1e6
        )
    })
}

/// `img` (8-bit, encoded in `space`) on a canvas of the border's colour.
pub fn add_border(img: &Rgba8, b: &Border, space: OutputSpace) -> Rgba8 {
    let (cw, ch, x0, y0) = b.layout(img.width, img.height);
    let [r, g, bl] = srgb8_in(space, b.color);
    let mut out = Rgba8::from_fn(cw, ch, |_, _| [r, g, bl, 255]);
    for y in 0..img.height {
        let (Some(src), Some(dst)) =
            (img.data.get(y * img.width..(y + 1) * img.width), out.data.get_mut((y0 + y) * cw + x0..(y0 + y) * cw + x0 + img.width))
        else {
            continue;
        };
        dst.copy_from_slice(src);
    }
    out
}

/// [`add_border`] for a high-bit-depth render (RGB samples: 16-bit encoded, or float linear).
pub fn add_border_deep(img: &DeepImage, b: &Border) -> DeepImage {
    let (cw, ch, x0, y0) = b.layout(img.width, img.height);
    let col = srgb8_in(img.space, b.color);
    let trc = img.space.trc();
    let samples = match &img.samples {
        DeepSamples::U16(v) => {
            let px = col.map(|c| u16::from(c) * 257);
            let mut out: Vec<u16> = std::iter::repeat_n(px, cw * ch).flatten().collect();
            place_rows(v, &mut out, img.width, img.height, cw, x0, y0);
            DeepSamples::U16(out)
        }
        DeepSamples::F32(v) => {
            let px = col.map(|c| trc.decode(f32::from(c) / 255.0));
            let mut out: Vec<f32> = std::iter::repeat_n(px, cw * ch).flatten().collect();
            place_rows(v, &mut out, img.width, img.height, cw, x0, y0);
            DeepSamples::F32(out)
        }
    };
    DeepImage { width: cw, height: ch, space: img.space, samples }
}

/// Copy a `w × h` RGB image's rows (`src`) into a canvas `cw` wide (`dst`) at `(x0, y0)`.
fn place_rows<T: Copy>(src: &[T], dst: &mut [T], w: usize, h: usize, cw: usize, x0: usize, y0: usize) {
    for y in 0..h {
        let (Some(s), Some(d)) = (src.get(y * w * 3..(y + 1) * w * 3), dst.get_mut(((y0 + y) * cw + x0) * 3..((y0 + y) * cw + x0 + w) * 3)) else {
            continue;
        };
        d.copy_from_slice(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_even_border_is_the_same_on_every_side() {
        let b = Border { thickness: 0.1, ..Default::default() };
        assert_eq!(b.layout(600, 400), (720, 520, 60, 60));
    }

    #[test]
    fn a_shaped_canvas_pads_the_short_way_and_keeps_the_thickness_on_the_narrow_sides() {
        // a landscape photo on a square: 30 px left and right, the rest above and below
        let b = Border { thickness: 0.05, ratio: Some([1, 1]), ..Default::default() };
        assert_eq!(b.layout(600, 400), (660, 660, 30, 130));
        // 4:5 portrait canvas round a landscape photo
        let (cw, ch, x, _) = Border { ratio: Some([4, 5]), ..b }.layout(600, 400);
        assert_eq!((cw, x), (660, 30));
        assert!((cw as f32 / ch as f32 - 0.8).abs() < 0.01, "{cw}×{ch}");
        // no thickness: only padding to the shape
        assert_eq!(Border { thickness: 0.0, ratio: Some([1, 1]), ..b }.layout(600, 400), (600, 600, 0, 100));
    }

    #[test]
    fn hostile_numbers_draw_something_sane() {
        let b = Border { thickness: f32::NAN, ratio: Some([0, 5]), ..Default::default() };
        assert_eq!(b.layout(10, 10), (10, 10, 0, 0));
        let b = Border { thickness: 99.0, ratio: Some([1000, 1]), ..Default::default() };
        let (cw, ch, ..) = b.layout(100, 100);
        assert_eq!((cw, ch), (150, 150), "thickness capped, the far-out shape ignored");
    }

    #[test]
    fn the_photo_lands_in_the_middle_of_the_colour() {
        let img = Rgba8::from_fn(4, 2, |_, _| [10, 20, 30, 255]);
        let b = Border { color: [200, 100, 50], thickness: 0.25, ratio: None, keep_size: false };
        let out = add_border(&img, &b, OutputSpace::Srgb);
        assert_eq!((out.width, out.height), (6, 4));
        assert_eq!(out.get(0, 0), [200, 100, 50, 255]);
        assert_eq!(out.get(1, 1), [10, 20, 30, 255]);
        assert_eq!(out.get(4, 2), [10, 20, 30, 255]);
        assert_eq!(out.get(5, 3), [200, 100, 50, 255]);
    }

    #[test]
    fn a_deep_render_gets_the_same_border() {
        let img = DeepImage { width: 2, height: 2, space: OutputSpace::Srgb, samples: DeepSamples::F32(vec![0.5; 12]) };
        let b = Border { color: [255, 255, 255], thickness: 0.5, ratio: None, keep_size: false };
        let out = add_border_deep(&img, &b);
        assert_eq!((out.width, out.height), (4, 4));
        let DeepSamples::F32(v) = out.samples else { panic!("float") };
        assert!((v[0] - 1.0).abs() < 1e-4, "white, linear");
        assert_eq!(v[(4 + 1) * 3], 0.5, "the photo");
        let img = DeepImage { width: 1, height: 1, space: OutputSpace::Srgb, samples: DeepSamples::U16(vec![1, 2, 3]) };
        let out = add_border_deep(&img, &Border { color: [0, 0, 0], thickness: 0.25, ratio: Some([2, 1]), keep_size: false });
        let DeepSamples::U16(v) = out.samples else { panic!("16-bit") };
        assert_eq!(v.len(), out.width * out.height * 3);
        assert!(v.chunks(3).any(|p| p == [1, 2, 3]));
    }

    #[test]
    fn keep_size_shrinks_the_photo_to_make_room() {
        // the reviewer's case: a 4000 × 6000 portrait, a quarter of the long edge, 4:1
        let wide = Border { thickness: 0.25, ratio: Some([4, 1]), ..Default::default() };
        let (cw, ch, _, _) = wide.layout(4000, 6000);
        assert_eq!((cw, ch), (36000, 9000), "324 MP as it was");
        let keep = Border { keep_size: true, ..wide };
        let (pw, ph) = keep.photo_size(4000, 6000);
        let (cw, ch, _, _) = keep.layout(pw, ph);
        let ratio = (cw * ch) as f64 / (4000.0 * 6000.0);
        assert!((0.99..=1.01).contains(&ratio), "the canvas has the photo's own pixels: {cw} × {ch}");
        assert!((pw as f64 / ph as f64 - 4000.0 / 6000.0).abs() < 0.01, "the photo keeps its shape: {pw} × {ph}");
        // an even border: the same pixels, the shape a little squarer (as the border is even)
        let even = Border { keep_size: true, ..Default::default() };
        let (pw, ph) = even.photo_size(6000, 4000);
        let (cw, ch, _, _) = even.layout(pw, ph);
        assert!(((cw * ch) as f64 / 24e6 - 1.0).abs() < 0.01, "{cw} × {ch}");
        assert_eq!(Border::default().photo_size(6000, 4000), (6000, 4000), "off: the photo at its size");
    }

    #[test]
    fn a_border_past_the_budget_is_refused_before_it_is_drawn() {
        let wide = Border { thickness: 0.25, ratio: Some([4, 1]), ..Default::default() };
        // 8-bit on a phone with 1 GB for full-resolution work (the canvas alone is 1.3 GB)
        let why = border_too_big(&wide, (4000, 6000), 4, 1_000_000_000).expect("refused");
        assert!(why.contains("36000 × 9000") && why.contains("324 MP"), "{why}");
        assert!(border_too_big(&Border::default(), (4000, 6000), 4, 1_000_000_000).is_none(), "a usual border fits");
        assert!(border_too_big(&wide, (4000, 6000), 4, 0).is_none(), "no budget: only the largest output counts");
        let panorama = Border { thickness: 0.25, ratio: Some([1, 4]), ..Default::default() };
        assert!(border_too_big(&panorama, (60000, 3000), 4, 0).is_some(), "past the largest output there is");
    }
}
