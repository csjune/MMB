//! The embedded icons. Each ICO file carries frames up to 256×256, but only
//! the frame Windows actually draws is decoded, which keeps a few kilobytes
//! of pixels in memory instead of 256 KB per icon.

use slint::Image;

use crate::windows_integration;

const APP_ICON_ICO: &[u8] = include_bytes!("../assets/app.ico");
const TRAY_ICON_LIGHT_ICO: &[u8] = include_bytes!("../assets/tray-light.ico");
const TRAY_ICON_DARK_ICO: &[u8] = include_bytes!("../assets/tray-dark.ico");

const ICO_HEADER_LEN: usize = 6;
const ICO_ENTRY_LEN: usize = 16;

pub(crate) struct Icons {
    pub(crate) app: Image,
    tray_light: Image,
    tray_dark: Image,
}

impl Icons {
    pub(crate) fn load() -> Self {
        let sizes = windows_integration::icon_sizes();
        Self {
            app: decode(APP_ICON_ICO, sizes.large),
            tray_light: decode(TRAY_ICON_LIGHT_ICO, sizes.small),
            tray_dark: decode(TRAY_ICON_DARK_ICO, sizes.small),
        }
    }

    pub(crate) fn tray(&self, dark_mode: bool) -> Image {
        if dark_mode {
            self.tray_dark.clone()
        } else {
            self.tray_light.clone()
        }
    }
}

fn decode(ico: &[u8], size: u32) -> Image {
    let frame = single_frame_ico(ico, size).expect("embedded icon should be a valid ICO file");
    Image::load_from_data(&frame, Some("ico")).expect("embedded icon frame should decode")
}

/// Copies one frame of an ICO file into an ICO file of its own: the smallest
/// frame at least `size` pixels wide, or the largest one if none is.
fn single_frame_ico(ico: &[u8], size: u32) -> Option<Vec<u8>> {
    let count = usize::from(u16::from_le_bytes(ico.get(4..6)?.try_into().ok()?));
    let entries = (0..count)
        .map(|index| {
            let start = ICO_HEADER_LEN + index * ICO_ENTRY_LEN;
            ico.get(start..start + ICO_ENTRY_LEN)
        })
        .collect::<Option<Vec<_>>>()?;
    // A stored width of 0 means 256.
    let width = |entry: &[u8]| match entry[0] {
        0 => 256,
        width => u32::from(width),
    };
    let entry = entries
        .iter()
        .filter(|entry| width(entry) >= size)
        .min_by_key(|entry| width(entry))
        .or_else(|| entries.iter().max_by_key(|entry| width(entry)))?;

    let len = usize::try_from(u32::from_le_bytes(entry[8..12].try_into().ok()?)).ok()?;
    let offset = usize::try_from(u32::from_le_bytes(entry[12..16].try_into().ok()?)).ok()?;
    let data = ico.get(offset..offset.checked_add(len)?)?;

    let mut frame = Vec::with_capacity(ICO_HEADER_LEN + ICO_ENTRY_LEN + len);
    frame.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    frame.extend_from_slice(&entry[..12]);
    frame.extend_from_slice(&((ICO_HEADER_LEN + ICO_ENTRY_LEN) as u32).to_le_bytes());
    frame.extend_from_slice(data);
    Some(frame)
}

#[cfg(test)]
mod tests {
    use slint::Image;

    use super::{APP_ICON_ICO, TRAY_ICON_DARK_ICO, TRAY_ICON_LIGHT_ICO, single_frame_ico};

    /// An ICO file whose frames are the given widths, each with one byte of
    /// fake data equal to its width.
    fn fake_ico(widths: &[u8]) -> Vec<u8> {
        let mut ico = vec![0, 0, 1, 0, widths.len() as u8, 0];
        let data_start = 6 + 16 * widths.len();
        for (index, &width) in widths.iter().enumerate() {
            ico.extend_from_slice(&[width, width, 0, 0, 1, 0, 32, 0]);
            ico.extend_from_slice(&1u32.to_le_bytes());
            ico.extend_from_slice(&((data_start + index) as u32).to_le_bytes());
        }
        ico.extend_from_slice(widths);
        ico
    }

    fn picked_width(ico: &[u8], size: u32) -> u8 {
        let frame = single_frame_ico(ico, size).expect("frame");
        assert_eq!(&frame[4..6], &[1, 0]);
        assert_eq!(u32::from_le_bytes(frame[18..22].try_into().unwrap()), 22);
        frame[22]
    }

    #[test]
    fn picks_the_smallest_frame_that_is_big_enough() {
        let ico = fake_ico(&[16, 32, 48, 0]);
        assert_eq!(picked_width(&ico, 16), 16);
        assert_eq!(picked_width(&ico, 20), 32);
        assert_eq!(picked_width(&ico, 32), 32);
        assert_eq!(picked_width(&ico, 200), 0);
    }

    #[test]
    fn falls_back_to_the_largest_frame() {
        let ico = fake_ico(&[16, 32]);
        assert_eq!(picked_width(&ico, 64), 32);
    }

    #[test]
    fn rejects_truncated_files() {
        let ico = fake_ico(&[16, 32]);
        assert_eq!(single_frame_ico(&ico[..ico.len() - 1], 32), None);
        assert_eq!(single_frame_ico(&ico[..10], 16), None);
    }

    #[test]
    fn every_embedded_icon_decodes_at_the_requested_size() {
        for ico in [APP_ICON_ICO, TRAY_ICON_LIGHT_ICO, TRAY_ICON_DARK_ICO] {
            for size in [16, 20, 24, 32, 48] {
                let frame = single_frame_ico(ico, size).expect("frame");
                let image = Image::load_from_data(&frame, Some("ico")).expect("decodes");
                assert_eq!(image.size().width, size);
            }
        }
    }
}
