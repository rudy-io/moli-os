//! What each LIFX light can do, by product id, and nothing more: its
//! name, colour or not, the colour temperature range (equal bounds: a
//! fixed white) and the ranges later firmware widened. Only these facts of
//! LIFX's public product list (vendor 1), restated by hand; its file is
//! not copied. Lighting products only (the Switch has no light).

/// One LIFX lighting product.
#[derive(Debug, PartialEq, Eq)]
pub struct Product {
    pub pid: u32,
    pub name: &'static str,
    pub color: bool,
    /// Kelvins, (min, max).
    pub kelvin: (u16, u16),
    /// From firmware (major, minor) on: another kelvin range.
    pub upgrades: &'static [(u16, u16, u16, u16)],
}

impl Product {
    /// The kelvin range with this firmware.
    #[must_use]
    pub fn kelvin(&self, firmware: Option<(u16, u16)>) -> (u16, u16) {
        let mut range = self.kelvin;
        if let Some(have) = firmware {
            for &(major, minor, min, max) in self.upgrades {
                if have >= (major, minor) {
                    range = (min, max);
                }
            }
        }
        range
    }
}

/// The product of this vendor and id, if it is a known LIFX light.
#[must_use]
pub fn find(vendor: u32, pid: u32) -> Option<&'static Product> {
    if vendor != 1 {
        return None;
    }
    PRODUCTS.iter().find(|p| p.pid == pid)
}

#[rustfmt::skip]
static PRODUCTS: &[Product] = &[
    Product { pid: 1, name: "Original", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 2, name: "Original 1000", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 3, name: "Color 650", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 10, name: "LIFX White 800 (LV)", color: false, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 11, name: "LIFX White 800 (HV)", color: false, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 12, name: "LIFX LCMv4 White OEM (A19 LV)", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 13, name: "LIFX LCMv4 White OEM (A19 HV)", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 14, name: "LIFX Carbon Prototype", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 15, name: "LIFX LCMv4 Color (A21)", color: true, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 18, name: "LIFX White 900 (BR30)", color: false, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 19, name: "LIFX White 900 (BR30)", color: false, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 20, name: "LIFX Color 1000 (BR30)", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 21, name: "LIFX LCMv4 Color (GU10)", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 22, name: "LIFX Color 1000", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 23, name: "LIFX (A19)", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 24, name: "LIFX (BR30)", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 25, name: "LIFX+ (A19)", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 26, name: "LIFX+ (BR30)", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 27, name: "LIFX (A19)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 28, name: "LIFX (BR30)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 29, name: "LIFX+ (A19)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 30, name: "LIFX+ (BR30)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 31, name: "LIFX Z", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 32, name: "LIFX Z", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 33, name: "LIFX Downlight", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 36, name: "LIFX DL", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 37, name: "LIFX DL", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 38, name: "LIFX Beam", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 39, name: "LIFX DL WW 700lm", color: false, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 40, name: "LIFX DL Colour 700lm", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 43, name: "LIFX (A19)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 44, name: "LIFX (BR30)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 45, name: "LIFX+ (A19)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 46, name: "LIFX+ (BR30)", color: true, kelvin: (2500, 9000), upgrades: &[(2, 80, 1500, 9000)] },
    Product { pid: 49, name: "LIFX Mini C", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 50, name: "LIFX Mini DD", color: false, kelvin: (1500, 4000), upgrades: &[(3, 42, 1500, 6500), (3, 70, 1500, 9000)] },
    Product { pid: 51, name: "LIFX Mini W", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 52, name: "LIFX GU10", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 53, name: "LIFX GU10", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 54, name: "LIFX GU10 White", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 55, name: "LIFX Tile", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 56, name: "LIFX Beam", color: true, kelvin: (2500, 9000), upgrades: &[] },
    Product { pid: 57, name: "LIFX Candle C", color: true, kelvin: (2500, 9000), upgrades: &[(3, 50, 1500, 9000)] },
    Product { pid: 59, name: "LIFX Mini C", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 60, name: "LIFX Mini DD", color: false, kelvin: (1500, 4000), upgrades: &[(3, 42, 1500, 6500), (3, 70, 1500, 9000)] },
    Product { pid: 61, name: "LIFX Mini W", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 62, name: "LIFX (A19) LCM3", color: true, kelvin: (2500, 9000), upgrades: &[(3, 50, 1500, 9000)] },
    Product { pid: 63, name: "LIFX (BR30) LCM3", color: true, kelvin: (2500, 9000), upgrades: &[(3, 50, 1500, 9000)] },
    Product { pid: 64, name: "LIFX+ (A19) LCM3", color: true, kelvin: (2500, 9000), upgrades: &[(3, 50, 1500, 9000)] },
    Product { pid: 65, name: "LIFX+ (BR30) LCM3", color: true, kelvin: (2500, 9000), upgrades: &[(3, 50, 1500, 9000)] },
    Product { pid: 66, name: "LIFX Mini W", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 67, name: "LIFX Candle WW", color: false, kelvin: (2200, 6500), upgrades: &[] },
    Product { pid: 68, name: "LIFX Candle C", color: true, kelvin: (2500, 9000), upgrades: &[(3, 50, 1500, 9000)] },
    Product { pid: 72, name: "LIFX A19", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 81, name: "LIFX Candle WW", color: false, kelvin: (2200, 6500), upgrades: &[] },
    Product { pid: 82, name: "LIFX Filament Clear", color: false, kelvin: (2200, 2200), upgrades: &[] },
    Product { pid: 85, name: "LIFX Filament Amber", color: false, kelvin: (2000, 2000), upgrades: &[] },
    Product { pid: 86, name: "LIFX Filament Smokey", color: false, kelvin: (2200, 2200), upgrades: &[] },
    Product { pid: 87, name: "LIFX White", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 88, name: "LIFX White", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 90, name: "LIFX Clean A19 1100lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 91, name: "LIFX Color 800lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 92, name: "LIFX Colour 1000lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 93, name: "LIFX Color A19 1100lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 94, name: "LIFX Color BR30 1100lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 96, name: "LIFX Candle WW", color: false, kelvin: (2200, 6500), upgrades: &[] },
    Product { pid: 97, name: "LIFX Colour A19 1200lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 98, name: "LIFX Colour BR30 1100lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 99, name: "LIFX Clean A19 1200lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 100, name: "LIFX Filament Clear", color: false, kelvin: (2200, 2200), upgrades: &[] },
    Product { pid: 101, name: "LIFX Filament Amber", color: false, kelvin: (2000, 2000), upgrades: &[] },
    Product { pid: 102, name: "LIFX Filament Smokey", color: false, kelvin: (2200, 2200), upgrades: &[] },
    Product { pid: 109, name: "LIFX NV A19 1100lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 110, name: "LIFX NV BR30 1100lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 111, name: "LIFX NV A19 1200lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 112, name: "LIFX NV BR30 1100lm", color: true, kelvin: (2500, 9000), upgrades: &[(3, 42, 1500, 9000)] },
    Product { pid: 113, name: "LIFX WW 800lm", color: false, kelvin: (1500, 4000), upgrades: &[(3, 42, 1500, 6500), (3, 70, 1500, 9000)] },
    Product { pid: 114, name: "LIFX WW 1000lm", color: false, kelvin: (1500, 4000), upgrades: &[(3, 42, 1500, 6500), (3, 70, 1500, 9000)] },
    Product { pid: 117, name: "LIFX Z", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 118, name: "LIFX Z", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 119, name: "LIFX Beam", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 120, name: "LIFX Beam", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 121, name: "LIFX DL", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 122, name: "LIFX DL", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 123, name: "LIFX Mini 3.1 Color US", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 124, name: "LIFX Mini 3.1 Color Intl", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 125, name: "LIFX Mini 3.1 WW US", color: false, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 126, name: "LIFX Mini 3.1 WW Intl", color: false, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 127, name: "LIFX Mini 3.1 W US", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 128, name: "LIFX Mini 3.1 W Intl", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 129, name: "LIFX Color 800", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 130, name: "LIFX Colour 1000", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 131, name: "LIFX White to Warm 800", color: false, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 132, name: "LIFX White to Warm 1000", color: false, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 133, name: "LIFX White 650", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 134, name: "LIFX White 800", color: false, kelvin: (2700, 2700), upgrades: &[] },
    Product { pid: 135, name: "LIFX GU10 Color US", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 136, name: "LIFX GU10 Color Intl", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 137, name: "LIFX Candle C", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 138, name: "LIFX Candle C", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 141, name: "LIFX Neon", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 142, name: "LIFX Neon", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 143, name: "LIFX String", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 144, name: "LIFX String", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 151, name: "LIFX Neon Outdoor", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 152, name: "LIFX Neon Outdoor", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 153, name: "LIFX PAR38 US", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 154, name: "LIFX PAR38 Intl", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 155, name: "LIFX Sunshine", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 156, name: "LIFX A21 1000lm Intl", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 161, name: "LIFX Neon Outdoor", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 162, name: "LIFX Neon Outdoor", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 163, name: "LIFX A19", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 164, name: "LIFX BR30", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 165, name: "LIFX A19 Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 166, name: "LIFX BR30 Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 167, name: "LIFX DL AU", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 168, name: "LIFX DL US", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 169, name: "LIFX A21", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 170, name: "LIFX A21", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 171, name: "LIFX Spot", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 172, name: "LIFX Spot", color: true, kelvin: (1500, 9000), upgrades: &[] },
    Product { pid: 173, name: "LIFX Path", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 174, name: "LIFX Path", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 175, name: "LIFX PAR38", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 176, name: "LIFX Ceiling", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 177, name: "LIFX Ceiling", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 178, name: "LIFX Downlight US", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 179, name: "LIFX Downlight US", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 180, name: "LIFX Downlight US", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 181, name: "LIFX Mini", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 182, name: "LIFX Mini", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 185, name: "LIFX Candle C", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 186, name: "LIFX Candle C Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 187, name: "LIFX Candle", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 188, name: "LIFX Candle Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 191, name: "LIFX Everyday A19", color: true, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 192, name: "LIFX Everyday A19 Intl", color: true, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 201, name: "LIFX Ceiling 13x26", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 202, name: "LIFX Ceiling 13x26 Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 203, name: "LIFX String", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 204, name: "LIFX String", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 205, name: "LIFX Neon", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 206, name: "LIFX Neon", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 207, name: "LIFX Everyday Lightstrip", color: true, kelvin: (2500, 6500), upgrades: &[] },
    Product { pid: 208, name: "LIFX Everyday Lightstrip Intl", color: true, kelvin: (2500, 6500), upgrades: &[] },
    Product { pid: 211, name: "LIFX Everyday Permanent Outdoor US", color: true, kelvin: (2700, 6500), upgrades: &[] },
    Product { pid: 213, name: "LIFX Permanent Outdoor", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 214, name: "LIFX Permanent Outdoor Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 215, name: "LIFX Candle C", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 216, name: "LIFX Candle C Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 217, name: "LIFX Tube", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 218, name: "LIFX Tube Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 219, name: "LIFX Luna", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 220, name: "LIFX Luna Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 221, name: "LIFX Spot Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 222, name: "LIFX Path Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 223, name: "LIFX DL", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 224, name: "LIFX DL Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 225, name: "LIFX PAR38 Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 229, name: "LIFX Path Intl", color: true, kelvin: (1000, 10000), upgrades: &[] },
    Product { pid: 265, name: "LIFX Ceiling 13\"", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 266, name: "LIFX Ceiling 13\" Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 267, name: "LIFX Mirror", color: true, kelvin: (1500, 9000), upgrades: &[(4, 200, 1000, 10000)] },
    Product { pid: 268, name: "LIFX Mirror Intl", color: true, kelvin: (1500, 9000), upgrades: &[(4, 148, 1000, 10000)] },
    Product { pid: 300, name: "LIFX Everyday Permanent Outdoor US", color: true, kelvin: (2700, 6500), upgrades: &[] },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whites_colours_and_upgrades() {
        let mini_w = find(1, 66).unwrap();
        assert!(!mini_w.color);
        assert_eq!(mini_w.kelvin(None), (2700, 2700));
        let white = find(1, 87).unwrap();
        assert_eq!(white.kelvin(Some((3, 90))), (2700, 2700));
        let mini_dd = find(1, 50).unwrap();
        assert_eq!(mini_dd.kelvin(Some((3, 10))), (1500, 4000));
        assert_eq!(mini_dd.kelvin(Some((3, 42))), (1500, 6500));
        assert_eq!(mini_dd.kelvin(Some((3, 80))), (1500, 9000));
        assert!(find(1, 1).unwrap().color);
        assert!(find(2, 1).is_none());
        assert!(find(1, 999_999).is_none());
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<u32> = PRODUCTS.iter().map(|p| p.pid).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PRODUCTS.len());
    }
}
