//! Tiny server-side HTML helpers. No template engine, no client-side
//! JavaScript: pages stay a few KB and render fine on low-end Android.

use axum::response::Html;

use crate::model::User;

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// Flash messages come from a fixed set of codes (`?m=code`), never from
/// user-controlled text, so they cannot be abused for content injection.
pub fn flash_text(code: &str) -> Option<&'static str> {
    Some(match code {
        "posted" => "Catatan terkirim.",
        "voted" => "Dukunganmu tercatat.",
        "moved" => "Dukunganmu dipindahkan.",
        "created" => "Ideologi berhasil didirikan.",
        "reported" => "Laporan terkirim. Terima kasih.",
        "appealed" => "Banding terkirim.",
        "deleted" => "Akunmu sudah dihapus.",
        "welcome" => "Akun dibuat. Selamat datang!",
        "done" => "Selesai.",
        _ => return None,
    })
}

pub fn page(title: &str, user: Option<&User>, flash: Option<&str>, body: &str) -> Html<String> {
    page_with(title, user, flash, body, false)
}

pub fn page_with(title: &str, user: Option<&User>, flash: Option<&str>, body: &str, wide: bool) -> Html<String> {
    let mut s = String::with_capacity(body.len() + 1800);
    s.push_str(
        "<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">",
    );
    s.push_str(&format!("<title>{} · RiverThink</title>", esc(title)));
    s.push_str("<link rel=\"stylesheet\" href=\"/s.css?v=2\"></head><body><header>");
    s.push_str("<a class=\"logo\" href=\"/\">RiverThink</a><nav>");
    s.push_str("<a href=\"/\">Ambinote</a><a href=\"/c/E\">#E</a><a href=\"/c/Z\">#Z</a><a href=\"/c/D\">#D</a><a href=\"/ideologies\">Ideologi</a>");
    s.push_str("<a href=\"/final-nine\">Final Nine</a><a href=\"/election\">Election</a><a href=\"/rules\">Aturan</a></nav><span class=\"me\">");
    match user {
        Some(u) => {
            s.push_str(&format!(
                "<a href=\"/me\"><span class=\"chip c-{c}\">{c}</span> {n}</a>",
                c = u.channel().code(),
                n = esc(&u.username)
            ));
            if u.is_admin() {
                s.push_str("<a href=\"/admin\">admin</a>");
            }
            s.push_str("<form class=\"inline\" method=\"post\" action=\"/logout\"><button>keluar</button></form>");
        }
        None => s.push_str("<a href=\"/login\">masuk</a><a href=\"/register\">daftar</a>"),
    }
    s.push_str("</span></header>");
    s.push_str(if wide { "<main class=\"wide\">" } else { "<main>" });
    if let Some(t) = flash.and_then(flash_text) {
        s.push_str(&format!("<div class=\"flash\">{t}</div>"));
    }
    s.push_str(body);
    s.push_str("</main><footer>RiverThink · MVP lokal · waktu ditampilkan dalam WIB</footer></body></html>");
    Html(s)
}

pub fn error_page(message: &str) -> Html<String> {
    page(
        "Oops",
        None,
        None,
        &format!(
            "<h1>Oops</h1><div class=\"flash err\">{}</div><p>Gunakan tombol kembali di browser untuk memperbaiki isian · <a href=\"/\">beranda</a></p>",
            esc(message)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html() {
        assert_eq!(esc("<script>\"x\" & 'y'</script>"), "&lt;script&gt;&quot;x&quot; &amp; &#39;y&#39;&lt;/script&gt;");
    }

    #[test]
    fn flash_is_whitelisted() {
        assert!(flash_text("posted").is_some());
        assert!(flash_text("<b>evil</b>").is_none());
    }
}
