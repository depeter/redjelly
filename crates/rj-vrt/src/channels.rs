//! The VRT live channels. Ids, logos and stream ids follow the Kodi add-on
//! (`resources/lib/data.py`); page slugs follow vrt.be.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    /// Slug of `https://www.vrt.be/vrtmax/livestream/video/{slug}/`.
    pub slug: &'static str,
    pub name: &'static str,
    /// Slug of `/vrtmax/tv-gids/{slug}/{date}/`, if VRT publishes a guide.
    pub guide: Option<&'static str>,
    /// media-items id; only used for the clear streams.
    pub stream_id: &'static str,
    /// Unencrypted HLS that mpv can play. Everything else is DRM and needs
    /// VRT's web player.
    pub clear: bool,
    pub logo: &'static str,
    /// Live screenshot VRT refreshes every few minutes.
    pub screenshot: Option<&'static str>,
}

pub const CHANNELS: &[Channel] = &[
    Channel {
        slug: "vrt1",
        name: "VRT 1",
        guide: Some("vrt1"),
        stream_id: "vualto_een_geo",
        clear: false,
        logo: "https://images.vrt.be/orig/2023/04/28/c448d669-e5c1-11ed-91d7-02b7b76bf47f.png",
        screenshot: Some("https://screenshots.vrt.be/w640hx/vrtnu-static/screenshots/een_auto.jpg"),
    },
    Channel {
        slug: "vrt-canvas",
        name: "VRT Canvas",
        guide: Some("vrt-canvas"),
        stream_id: "vualto_canvas_geo",
        clear: false,
        logo: "https://images.vrt.be/orig/logo/canvas/CANVAS_logo_lichtblauw.jpg",
        screenshot: Some("https://screenshots.vrt.be/w640hx/vrtnu-static/screenshots/canvas_auto.jpg"),
    },
    Channel {
        slug: "ketnet",
        name: "Ketnet",
        guide: Some("ketnet"),
        stream_id: "vualto_ketnet_geo",
        clear: false,
        logo: "https://images.vrt.be/orig/logo/ketnet/ketnet_LOGO_rood_geel.png",
        screenshot: Some("https://screenshots.vrt.be/w640hx/vrtnu-static/screenshots/ketnet_auto.jpg"),
    },
    Channel {
        slug: "ketnet-jr",
        name: "Ketnet Junior",
        guide: None,
        stream_id: "ketnet_jr",
        clear: true,
        logo: "https://images.vrt.be/orig/2019/07/19/c309360a-aa10-11e9-abcc-02b7b76bf47f.png",
        screenshot: Some("https://screenshots.vrt.be/w640hx/vrtnu-static/screenshots/ketnet_jr.jpg"),
    },
    Channel {
        slug: "sporza-live",
        name: "Sporza",
        guide: None,
        stream_id: "vualto_sporza_geo",
        clear: false,
        logo: "https://images.vrt.be/orig/logo/sporza/sporza_logo_zwart.png",
        screenshot: Some("https://screenshots.vrt.be/w640hx/vrtnu-static/screenshots/sporza_auto.jpg"),
    },
    Channel {
        slug: "vrt-nws",
        name: "VRT NWS",
        guide: None,
        stream_id: "vualto_nieuws",
        clear: true,
        logo: "https://images.vrt.be/orig/logos/vrtnws.png",
        screenshot: None,
    },
];

impl Channel {
    pub fn by_slug(slug: &str) -> Option<&'static Channel> {
        CHANNELS.iter().find(|c| c.slug == slug)
    }

    /// The channel a `/vrtmax/livestream/video/{slug}/` link points at.
    pub fn from_link(link: &str) -> Option<&'static Channel> {
        let slug = link.trim_matches('/').strip_prefix("vrtmax/livestream/video/")?;
        Self::by_slug(slug.trim_matches('/'))
    }

    pub fn page_url(&self) -> String {
        format!("{}/vrtmax/livestream/video/{}/", crate::SITE, self.slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert_eq!(Channel::from_link("/vrtmax/livestream/video/vrt1/").unwrap().name, "VRT 1");
        assert_eq!(Channel::from_link("/vrtmax/a-z/thuis/"), None);
        assert_eq!(CHANNELS.iter().filter(|c| c.clear).count(), 2);
        assert_eq!(Channel::by_slug("ketnet").unwrap().page_url(), "https://www.vrt.be/vrtmax/livestream/video/ketnet/");
    }
}
