use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use super::html::{esc, page, page_with};
use super::{AppState, MaybeUser, RequireUser, client_ip, clear_cookie, session_cookie, token_from};
use crate::domain::calendar::{ELECTION_WEEKS, format_wib};
use crate::domain::channel::Channel;
use crate::domain::election::{SEATS_PER_CHANNEL, STAGE_CUTOFFS};
use crate::domain::vote::VoteDecision;
use crate::error::{AppError, AppResult};
use crate::model::User;
use crate::service::election::NineRow;
use crate::service::ideology::IdeologyRow;
use crate::service::notes::{self, NoteView};
use crate::service::{auth, election, ideology, moderation, vote};

const PAGE_SIZE: i64 = 20;

fn ok_html(h: Html<String>) -> AppResult<Response> {
    Ok(h.into_response())
}

fn to(path: &str, flash: &str) -> Response {
    let sep = if path.contains('?') { '&' } else { '?' };
    Redirect::to(&format!("{path}{sep}m={flash}")).into_response()
}

fn limit(st: &AppState, headers: &HeaderMap, addr: SocketAddr, key: &'static str, n: u32, window: i64) -> AppResult<()> {
    let ip = client_ip(headers, addr, st.cfg.trust_proxy);
    if st.limiter.allow(ip, key, n, window, st.clock.now()) {
        Ok(())
    } else {
        Err(AppError::RateLimited)
    }
}

// ---- shared fragments ------------------------------------------------------------------

fn ch_class(code: &str) -> &'static str {
    match code {
        "E" => "c-E",
        "Z" => "c-Z",
        _ => "c-D",
    }
}

fn report_form(kind: &str, id: i64) -> String {
    format!(
        "<details class=\"rep\"><summary>laporkan</summary>\
         <form method=\"post\" action=\"/report\"><input type=\"hidden\" name=\"kind\" value=\"{kind}\">\
         <input type=\"hidden\" name=\"id\" value=\"{id}\">\
         <input type=\"text\" name=\"reason\" placeholder=\"alasan (3-500 karakter)\" required minlength=\"3\" maxlength=\"500\">\
         <button class=\"alt\">kirim laporan</button></form></details>"
    )
}

/// A chat bubble: colour comes from the author's channel, the small label is the
/// ideology the author currently supports (ideology identity = colour + label only).
fn bubble(n: &NoteView, me: Option<&User>) -> String {
    let mut s = format!("<article class=\"bubble {}\">", ch_class(&n.channel));
    s.push_str(&format!(
        "<div class=\"meta\"><span class=\"chip\">{ch}</span> <b>{a}</b>",
        ch = esc(&n.channel),
        a = esc(&n.author)
    ));
    if let Some(i) = &n.ideology {
        s.push_str(&format!("<span class=\"ide\">{}</span>", esc(i)));
    }
    s.push_str(&format!(" <small>{}</small>", format_wib(n.created_at)));
    if let Some(r) = n.room.as_deref().filter(|r| Channel::parse(r).is_some()) {
        s.push_str(&format!(" <a class=\"room\" href=\"/c/{r}\">#{r}</a>"));
    }
    s.push_str("</div>");
    s.push_str(&format!("<h3>{}</h3>", esc(&n.title)));
    if !n.body.is_empty() {
        s.push_str(&format!("<p>{}</p>", esc(&n.body)));
    }
    s.push_str("<div class=\"foot\">");
    for t in n.tag_list() {
        s.push_str(&format!("<a href=\"/tag/{t}\">#{t}</a>", t = esc(t)));
    }
    if me.is_some_and(|u| u.id != n.author_id) {
        s.push_str(&report_form("note", n.id));
    }
    s.push_str("</div></article>");
    s
}

fn note_form(selected: Option<Channel>) -> String {
    let opt = |c: Channel| {
        format!(
            "<option value=\"{c}\"{sel}>#{c}</option>",
            c = c.code(),
            sel = if selected == Some(c) { " selected" } else { "" }
        )
    };
    format!(
        "<form class=\"card\" method=\"post\" action=\"/notes\"><b>Tulis ambisimu</b>\
         <input type=\"text\" name=\"title\" placeholder=\"judul singkat, boleh pakai #tag\" required maxlength=\"120\">\
         <textarea name=\"body\" placeholder=\"isi (opsional, maks 2000 karakter) — tag: #agritech #robotics\" maxlength=\"2000\"></textarea>\
         <label>Kirim ke <select name=\"room\"><option value=\"\">Umum (semua)</option>{}{}{}</select></label>\
         <button>Kirim</button></form>",
        opt(Channel::E),
        opt(Channel::Z),
        opt(Channel::D)
    )
}

// ---- Ambinote -------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct FeedQ {
    before: Option<i64>,
    m: Option<String>,
}

async fn feed_page(st: &AppState, user: Option<User>, tag: Option<String>, room: Option<Channel>, q: FeedQ) -> AppResult<Response> {
    let mut rows = notes::list(&st.db, tag.as_deref(), room, q.before, PAGE_SIZE + 1).await?;
    let more = rows.len() as i64 > PAGE_SIZE;
    rows.truncate(PAGE_SIZE as usize);
    let mut b = String::new();
    match (&tag, room) {
        (Some(t), _) => b.push_str(&format!("<h1>#{}</h1><p><a href=\"/\">&larr; semua Ambinote</a></p>", esc(t))),
        (None, Some(r)) => {
            b.push_str(&format!(
                "<h1 class=\"{cls}\" style=\"border-left:6px solid var(--cb);padding-left:.5rem\">#{c}</h1>\
                 <p class=\"mut\">Ruang diskusi terbuka: siapa pun, dari channel mana pun, boleh membaca dan menulis di sini. \
                 Channel asal hanya menentukan ideologi yang bisa kamu dukung. Warna gelembung = channel asal penulis.</p>",
                cls = ch_class(r.code()),
                c = r.code()
            ));
            if user.is_some() {
                b.push_str(&note_form(Some(r)));
            } else {
                b.push_str("<p><a href=\"/login\">Masuk</a> atau <a href=\"/register\">daftar</a> untuk menulis.</p>");
            }
        }
        (None, None) => {
            b.push_str("<h1>Ambinote</h1><p class=\"mut\">Papan ambisi publik, semua ruang jadi satu. Warna gelembung = channel asal penulis (#E biru, #Z hijau, #D oranye); tulisan kecil = ideologi yang ia dukung.</p>");
            if user.is_some() {
                b.push_str(&note_form(None));
            } else {
                b.push_str("<p><a href=\"/login\">Masuk</a> atau <a href=\"/register\">daftar</a> untuk menulis.</p>");
            }
        }
    }
    if rows.is_empty() {
        b.push_str("<p class=\"mut\">Belum ada catatan.</p>");
    }
    for n in &rows {
        b.push_str(&bubble(n, user.as_ref()));
    }
    if more {
        if let Some(last) = rows.last() {
            let base = match (tag.as_deref(), room) {
                (Some(t), _) => format!("/tag/{t}"),
                (None, Some(r)) => format!("/c/{}", r.code()),
                (None, None) => "/".to_string(),
            };
            b.push_str(&format!("<p class=\"pager\"><a href=\"{base}?before={}\">Lebih lama &rarr;</a></p>", last.id));
        }
    }
    ok_html(page("Ambinote", user.as_ref(), q.m.as_deref(), &b))
}

pub async fn feed(State(st): State<AppState>, MaybeUser(u): MaybeUser, Query(q): Query<FeedQ>) -> AppResult<Response> {
    feed_page(&st, u, None, None, q).await
}

pub async fn room(State(st): State<AppState>, MaybeUser(u): MaybeUser, Path(code): Path<String>, Query(q): Query<FeedQ>) -> AppResult<Response> {
    let ch = Channel::parse(&code).ok_or(AppError::NotFound)?;
    feed_page(&st, u, None, Some(ch), q).await
}

pub async fn tag(State(st): State<AppState>, MaybeUser(u): MaybeUser, Path(tag): Path<String>, Query(q): Query<FeedQ>) -> AppResult<Response> {
    let tag = notes::normalize_tag(&tag).ok_or(AppError::NotFound)?;
    feed_page(&st, u, Some(tag), None, q).await
}

#[derive(Deserialize)]
pub struct NoteForm {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    room: String,
}

pub async fn post_note(State(st): State<AppState>, RequireUser(u): RequireUser, Form(f): Form<NoteForm>) -> AppResult<Response> {
    let room = match f.room.trim() {
        "" => None,
        r => Some(Channel::parse(r).ok_or_else(|| AppError::Invalid("Ruang tidak dikenal.".into()))?),
    };
    notes::post(&st.db, st.clock.now(), &u, &f.title, &f.body, room).await?;
    Ok(match room {
        Some(r) => to(&format!("/c/{}", r.code()), "posted"),
        None => to("/", "posted"),
    })
}

// ---- accounts ---------------------------------------------------------------------------------

pub async fn register_form(MaybeUser(u): MaybeUser) -> AppResult<Response> {
    let b = "<h1>Daftar</h1><form class=\"card\" method=\"post\" action=\"/register\">\
        <label>Username<input type=\"text\" name=\"username\" required minlength=\"3\" maxlength=\"24\" autocomplete=\"username\"></label>\
        <label>Password (min 8)<input type=\"password\" name=\"password\" required minlength=\"8\" maxlength=\"128\" autocomplete=\"new-password\"></label>\
        <label>Nomor telepon<input type=\"tel\" name=\"phone\" required maxlength=\"20\" inputmode=\"tel\" placeholder=\"08123456789\" autocomplete=\"tel\"></label>\
        <p class=\"mut\">Satu nomor hanya untuk 1 akun. Ingin ganti akun? Hapus akun lama di halaman profil. Nomor disimpan hanya sebagai hash, tidak ditampilkan.</p>\
        <label>Channel<select name=\"channel\"><option value=\"E\">#E</option><option value=\"Z\">#Z</option><option value=\"D\">#D</option></select></label>\
        <p class=\"mut\">Channel asalmu <b>tidak bisa diganti</b>. Ia hanya menentukan ideologi mana yang bisa kamu dukung dan dirikan. Untuk diskusi kamu bebas membaca dan menulis di channel mana pun. Ketiga channel setara.</p>\
        <button>Buat akun</button></form>";
    ok_html(page("Daftar", u.as_ref(), None, b))
}

#[derive(Deserialize)]
pub struct RegisterForm {
    username: String,
    password: String,
    channel: String,
    #[serde(default)]
    phone: String,
}

pub async fn register(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<RegisterForm>,
) -> AppResult<Response> {
    limit(&st, &headers, addr, "register", 5, 3600)?;
    let now = st.clock.now();
    auth::register(&st.db, &st.cfg, now, &f.username, &f.password, &f.channel, Some(&f.phone)).await?;
    let token = auth::login(&st.db, &st.cfg, now, &f.username, &f.password).await?;
    let mut res = to("/", "welcome");
    set_cookie(&mut res, &session_cookie(&token, &st.cfg));
    Ok(res)
}

fn set_cookie(res: &mut Response, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
}

pub async fn login_form(MaybeUser(u): MaybeUser) -> AppResult<Response> {
    let b = "<h1>Masuk</h1><form class=\"card\" method=\"post\" action=\"/login\">\
        <label>Username<input type=\"text\" name=\"username\" required autocomplete=\"username\"></label>\
        <label>Password<input type=\"password\" name=\"password\" required autocomplete=\"current-password\"></label>\
        <button>Masuk</button></form>";
    ok_html(page("Masuk", u.as_ref(), None, b))
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

pub async fn login(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<LoginForm>,
) -> AppResult<Response> {
    limit(&st, &headers, addr, "login", 10, 300)?;
    let token = auth::login(&st.db, &st.cfg, st.clock.now(), &f.username, &f.password).await?;
    let mut res = Redirect::to("/").into_response();
    set_cookie(&mut res, &session_cookie(&token, &st.cfg));
    Ok(res)
}

pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(t) = token_from(&headers) {
        auth::logout(&st.db, t).await?;
    }
    let mut res = Redirect::to("/").into_response();
    set_cookie(&mut res, &clear_cookie());
    Ok(res)
}

// ---- ideologies ---------------------------------------------------------------------------------

fn ide_row(i: &IdeologyRow, me: Option<&User>) -> String {
    let mut s = format!(
        "<tr><td><a href=\"/ideologies/{id}\">{n}</a>{nine}{rs}<br><small>gen {g} · oleh {c}</small></td>\
         <td class=\"n\">{sup}</td><td class=\"n\">{act}</td>",
        id = i.id,
        n = esc(&i.name),
        nine = if i.in_nine_now == 1 { " <span class=\"badge\">Final Nine</span>" } else { "" },
        rs = if i.status != "active" { " <span class=\"badge\">dibatasi</span>" } else { "" },
        g = i.founded_generation,
        c = esc(&i.creator),
        sup = i.support,
        act = i.activity
    );
    if me.is_some_and(|u| u.channel == i.channel) && i.status == "active" {
        s.push_str(&format!(
            "<td><form class=\"inline\" method=\"post\" action=\"/ideologies/{}/vote\"><button>dukung</button></form></td>",
            i.id
        ));
    } else {
        s.push_str("<td></td>");
    }
    s.push_str("</tr>");
    s
}

pub async fn ideologies(State(st): State<AppState>, MaybeUser(u): MaybeUser, Query(q): Query<FeedQ>) -> AppResult<Response> {
    let now = st.clock.now();
    let mut b = String::from(
        "<h1>Ideologi</h1><p class=\"mut\">Dukungan = jumlah akun aktif yang kini mendukung. Aktivitas = catatan Ambinote 30 hari terakhir dari pendukung. \
         Keduanya bukan skor gabungan dan bukan hitungan manusia unik.</p>",
    );
    let board = st.board().await?;
    b.push_str("<div class=\"cols\">");
    for (ch, rows, info) in board.iter() {
        let ch = *ch;
        b.push_str(&format!(
            "<section class=\"col {cls}\"><h2>#{c}</h2><p><small>Kuota pendirian gen {g}: {used}/{cap} ({au} pengguna aktif)</small></p>",
            cls = ch_class(ch.code()),
            c = ch.code(),
            g = info.generation,
            used = info.used,
            cap = info.cap,
            au = info.active_users
        ));
        if rows.is_empty() {
            b.push_str("<p class=\"mut\">Belum ada ideologi.</p>");
        } else {
            b.push_str("<table><tr><th>Nama</th><th class=\"n\">Dukung</th><th class=\"n\">Aktif</th><th></th></tr>");
            for r in rows {
                b.push_str(&ide_row(r, u.as_ref()));
            }
            b.push_str("</table>");
        }
        b.push_str("</section>");
    }
    b.push_str("</div>");
    if let Some(user) = &u {
        let info = ideology::capacity_info_pool(&st.db, &st.cfg, now, user.channel()).await?;
        b.push_str(&format!(
            "<h2>Dirikan ideologi di channel #{c}</h2>",
            c = user.channel().code()
        ));
        if info.is_full() {
            b.push_str(&format!("<p class=\"mut\">Kuota channel #{} generasi {} penuh ({}/{}).</p>", user.channel().code(), info.generation, info.used, info.cap));
        } else {
            b.push_str(
                "<form class=\"card\" method=\"post\" action=\"/ideologies\">\
                 <label>Nama (kamu yang menentukan)<input type=\"text\" name=\"name\" required minlength=\"3\" maxlength=\"40\" placeholder=\"mis. Capitalist Technocracy\"></label>\
                 <label>Visi dan prinsip<textarea name=\"description\" required minlength=\"10\" maxlength=\"2000\"></textarea></label>\
                 <p class=\"mut\">Satu ideologi per akun per generasi. Pendiri tidak otomatis menjadi pemimpin.</p>\
                 <button>Dirikan</button></form>",
            );
        }
    }
    ok_html(page_with("Ideologi", u.as_ref(), q.m.as_deref(), &b, true))
}

#[derive(Deserialize)]
pub struct IdeologyForm {
    name: String,
    description: String,
}

pub async fn create_ideology(State(st): State<AppState>, RequireUser(u): RequireUser, Form(f): Form<IdeologyForm>) -> AppResult<Response> {
    let id = ideology::create(&st.db, &st.cfg, st.clock.now(), &u, &f.name, &f.description).await?;
    Ok(to(&format!("/ideologies/{id}"), "created"))
}

pub async fn ideology_detail(State(st): State<AppState>, MaybeUser(u): MaybeUser, Path(id): Path<i64>, Query(q): Query<FeedQ>) -> AppResult<Response> {
    let now = st.clock.now();
    let i = ideology::get(&st.db, &st.cfg, now, id).await?.ok_or(AppError::NotFound)?;
    let mut b = format!(
        "<article class=\"card {cls}\" style=\"border-left:6px solid var(--cb)\"><h1>{n}</h1>\
         <p><span class=\"chip\">{c}</span> channel #{c} · didirikan gen {g} oleh {cr} pada {t}</p>\
         <p style=\"white-space:pre-wrap\">{d}</p>",
        cls = ch_class(&i.channel),
        n = esc(&i.name),
        c = esc(&i.channel),
        g = i.founded_generation,
        cr = esc(&i.creator),
        t = format_wib(i.created_at),
        d = esc(&i.description)
    );
    b.push_str(&format!(
        "<div class=\"stats\"><div><b>{}</b>dukungan</div><div><b>{}</b>aktivitas 30 hari</div><div><b>{}</b>status Final Nine</div></div>",
        i.support,
        i.activity,
        match (i.in_nine_now, i.last_nine_gen) {
            (1, _) => "di Final Nine".to_string(),
            (_, Some(g)) => format!("terakhir gen {g}"),
            _ => "belum pernah".to_string(),
        }
    ));
    if i.status != "active" {
        b.push_str("<p class=\"flash err\">Ideologi ini sedang dibatasi moderator.</p>");
    }
    if let Some(user) = &u {
        if user.channel == i.channel {
            let decision = vote::preview(&st.db, &st.cfg, now, user.id, i.id).await?;
            let note = match decision {
                VoteDecision::Allow => String::new(),
                VoteDecision::AlreadySupporting => "<p class=\"mut\">Kamu sudah mendukung ideologi ini.</p>".into(),
                VoteDecision::Cooldown { until } => format!("<p class=\"mut\">Pindah dukungan baru bisa lagi pada {}.</p>", format_wib(until)),
                VoteDecision::LockedFinalWeek => "<p class=\"mut\">Vote dikunci selama minggu final election.</p>".into(),
            };
            b.push_str(&note);
            if decision == VoteDecision::Allow && i.status == "active" {
                b.push_str(&format!("<form method=\"post\" action=\"/ideologies/{}/vote\"><button>Dukung ideologi ini</button></form>", i.id));
            }
        } else {
            b.push_str(&format!("<p class=\"mut\">Channel asalmu #{}; ideologi ini hanya bisa didukung dari channel #{}. Kamu tetap bebas berdiskusi di mana pun.</p>", user.channel().code(), esc(&i.channel)));
        }
        if user.id != i.creator_id {
            b.push_str(&report_form("ideology", i.id));
        }
    }
    b.push_str("</article><p><a href=\"/ideologies\">&larr; semua ideologi</a></p>");
    ok_html(page(&i.name, u.as_ref(), q.m.as_deref(), &b))
}

pub async fn cast_vote(State(st): State<AppState>, RequireUser(u): RequireUser, Path(id): Path<i64>) -> AppResult<Response> {
    let flash = match vote::cast(&st.db, &st.cfg, st.clock.now(), u.id, id).await? {
        vote::VoteOutcome::Cast => "voted",
        vote::VoteOutcome::Moved => "moved",
    };
    Ok(to(&format!("/ideologies/{id}"), flash))
}

// ---- Final Nine / election -----------------------------------------------------------------------

fn nine_table(rows: &[NineRow], live: bool) -> String {
    let mut s = String::from("<div class=\"cols\">");
    for ch in Channel::ALL {
        s.push_str(&format!("<section class=\"col {}\"><h3>#{}</h3>", ch_class(ch.code()), ch.code()));
        let mine: Vec<_> = rows.iter().filter(|r| r.channel == ch.code()).collect();
        if mine.is_empty() {
            s.push_str("<p class=\"mut\">Tidak ada pemenang.</p>");
        } else {
            s.push_str("<table><tr><th>#</th><th>Ideologi</th><th class=\"n\">Final</th>");
            if live {
                s.push_str("<th class=\"n\">Kini</th>");
            }
            s.push_str("</tr>");
            for r in mine {
                s.push_str(&format!(
                    "<tr><td>{}</td><td><a href=\"/ideologies/{}\">{}</a></td><td class=\"n\">{}</td>",
                    r.slot,
                    r.ideology_id,
                    esc(&r.name),
                    r.support_final
                ));
                if live {
                    s.push_str(&format!("<td class=\"n\">{}</td>", r.support_now));
                }
                s.push_str("</tr>");
            }
            s.push_str("</table>");
        }
        s.push_str("</section>");
    }
    s.push_str("</div>");
    s
}

pub async fn final_nine(State(st): State<AppState>, MaybeUser(u): MaybeUser) -> AppResult<Response> {
    let hist = election::history(&st.db).await?;
    let mut b = String::from("<h1>Final Nine</h1>");
    match hist.first() {
        None => b.push_str("<p class=\"mut\">Belum ada generasi. Final Nine pertama ditetapkan di akhir election genesis (lihat <a href=\"/election\">Election</a>).</p>"),
        Some(cur) => {
            b.push_str(&format!(
                "<p>Generasi <b>{}</b> · {} s/d {}</p>",
                cur.generation.number,
                format_wib(cur.generation.starts_at),
                format_wib(cur.generation.ends_at)
            ));
            b.push_str(&nine_table(&cur.rows, true));
            b.push_str("<p class=\"mut\">\"Final\" = dukungan saat seleksi final. \"Kini\" = dukungan sekarang. Posisi di Final Nine adalah status kompetitif, bukan jabatan administratif.</p>");
            if hist.len() > 1 {
                b.push_str("<h2>Riwayat</h2>");
                for h in hist.iter().skip(1) {
                    b.push_str(&format!("<h3>Generasi {} · {} s/d {}</h3>", h.generation.number, format_wib(h.generation.starts_at), format_wib(h.generation.ends_at)));
                    b.push_str(&nine_table(&h.rows, false));
                }
            }
        }
    }
    ok_html(page_with("Final Nine", u.as_ref(), None, &b, true))
}

pub async fn election(State(st): State<AppState>, MaybeUser(u): MaybeUser) -> AppResult<Response> {
    let now = st.clock.now();
    let status = election::status(&st.db, now).await?;
    let mut b = String::from("<h1>Election</h1>");
    match status.running {
        Some(r) => {
            b.push_str(&format!(
                "<p>Election untuk <b>generasi {}</b>: {} &rarr; {}</p>",
                r.target_generation,
                format_wib(r.starts_at),
                format_wib(r.ends_at)
            ));
            b.push_str(&format!(
                "<div class=\"stats\"><div><b>{}/{}</b>seleksi selesai</div><div><b>{}</b>seleksi berikut</div><div><b>{}</b>vote</div></div>",
                r.stage,
                ELECTION_WEEKS,
                if r.stage >= ELECTION_WEEKS { "—".into() } else { format_wib(r.next_selection_at) },
                if r.votes_locked { "dikunci" } else { "terbuka" }
            ));
            if r.target_generation == 1 {
                b.push_str("<p class=\"mut\">Election genesis: ideologi yang berdiri sepanjang minggu pertama masuk daftar kandidat saat seleksi 1.</p>");
            }
            b.push_str("<div class=\"cols\">");
            for ch in Channel::ALL {
                b.push_str(&format!("<section class=\"col {}\"><h3>#{}</h3><table>", ch_class(ch.code()), ch.code()));
                for e in r.entries.iter().filter(|e| e.channel == ch.code()) {
                    let out = e.eliminated_stage.map(|s| format!(" <span class=\"badge\">gugur s{s}</span>")).unwrap_or_default();
                    let inc = if e.was_incumbent == 1 { " <span class=\"badge\">incumbent</span>" } else { "" };
                    let cls = if e.eliminated_stage.is_some() { " class=\"out\"" } else { "" };
                    b.push_str(&format!(
                        "<tr><td{cls}><a href=\"/ideologies/{}\">{}</a></td><td>{inc}{out}</td><td class=\"n\">{}</td></tr>",
                        e.ideology_id,
                        esc(&e.name),
                        e.support_now
                    ));
                }
                b.push_str("</table></section>");
            }
            b.push_str("</div>");
        }
        None => match status.next_election_at {
            Some(t) => b.push_str(&format!("<p>Tidak ada election berjalan. Election berikutnya mulai <b>{}</b>.</p>", format_wib(t))),
            None => b.push_str("<p>Election genesis akan dimulai otomatis begitu server berjalan.</p>"),
        },
    }
    b.push_str(&format!(
        "<h2>Mekanisme seleksi</h2><p>Setiap minggu, per channel secara terpisah, ideologi diurutkan menurut dukungan (seri: yang lebih tua, lalu id lebih kecil). \
         Lolos seleksi 1/2/3/4: {}/{}/{}/{} teratas; seleksi 4 menetapkan {} kursi dengan syarat dukungan minimal 1. Incumbent tidak punya kekebalan.</p>",
        STAGE_CUTOFFS[0], STAGE_CUTOFFS[1], STAGE_CUTOFFS[2], STAGE_CUTOFFS[3], SEATS_PER_CHANNEL
    ));
    ok_html(page_with("Election", u.as_ref(), None, &b, true))
}

// ---- profile, reports, appeals -------------------------------------------------------------------

pub async fn me(State(st): State<AppState>, RequireUser(u): RequireUser, Query(q): Query<FeedQ>) -> AppResult<Response> {
    let mut b = format!("<h1>{}</h1><p><span class=\"chip {}\">{}</span> channel asal #{} · bergabung {}</p>", esc(&u.username), ch_class(u.channel().code()), u.channel().code(), u.channel().code(), format_wib(u.created_at));
    match vote::current(&st.db, u.id).await? {
        Some(v) => {
            let until = v.cast_at + st.cfg.vote_cooldown_secs;
            b.push_str(&format!("<p>Kamu mendukung <a href=\"/ideologies/{}\">{}</a> sejak {}. Pindah dukungan bisa mulai {}.</p>", v.ideology_id, esc(&v.name), format_wib(v.cast_at), format_wib(until)));
        }
        None => b.push_str("<p>Kamu belum mendukung ideologi mana pun. <a href=\"/ideologies\">Pilih satu</a>.</p>"),
    }
    let m = moderation::my_moderation(&st.db, &u).await?;
    if m.account_restricted || !m.hidden_notes.is_empty() || !m.restricted_ideologies.is_empty() {
        b.push_str("<h2>Tindakan moderasi</h2>");
        let form = |kind: &str, id: i64, label: &str| {
            format!(
                "<form class=\"card\" method=\"post\" action=\"/appeal\"><b>{label}</b>\
                 <input type=\"hidden\" name=\"kind\" value=\"{kind}\"><input type=\"hidden\" name=\"id\" value=\"{id}\">\
                 <input type=\"text\" name=\"message\" placeholder=\"alasan bandingmu\" required minlength=\"3\" maxlength=\"1000\">\
                 <button class=\"alt\">ajukan banding</button></form>"
            )
        };
        if m.account_restricted {
            b.push_str(&form("user", u.id, "Akunmu dibatasi"));
        }
        for (id, t) in &m.hidden_notes {
            b.push_str(&form("note", *id, &format!("Catatan disembunyikan: {}", esc(t))));
        }
        for (id, n) in &m.restricted_ideologies {
            b.push_str(&form("ideology", *id, &format!("Ideologi dibatasi: {}", esc(n))));
        }
    }
    b.push_str("<h2>Hapus akun</h2><form class=\"card\" method=\"post\" action=\"/me/delete\">\
        <p class=\"mut\">Dukunganmu ditarik, sesi berakhir, dan nomor teleponmu bebas dipakai akun baru. Username berubah menjadi <code>deleted_N</code>; catatan dan ideologi yang pernah kamu buat tetap ada. Tidak bisa dibatalkan.</p>\
        <label>Password<input type=\"password\" name=\"password\" required autocomplete=\"current-password\"></label>\
        <button class=\"alt\">Hapus akun saya</button></form>");
    ok_html(page("Profil", Some(&u), q.m.as_deref(), &b))
}

#[derive(Deserialize)]
pub struct DeleteForm {
    password: String,
}

pub async fn delete_me(State(st): State<AppState>, RequireUser(u): RequireUser, Form(f): Form<DeleteForm>) -> AppResult<Response> {
    auth::delete_account(&st.db, st.clock.now(), u.id, &f.password).await?;
    let mut res = to("/", "deleted");
    set_cookie(&mut res, &clear_cookie());
    Ok(res)
}

#[derive(Deserialize)]
pub struct ReportForm {
    kind: String,
    id: i64,
    reason: String,
}

pub async fn report(State(st): State<AppState>, RequireUser(u): RequireUser, Form(f): Form<ReportForm>) -> AppResult<Response> {
    moderation::report(&st.db, st.clock.now(), &u, &f.kind, f.id, &f.reason).await?;
    Ok(to("/", "reported"))
}

#[derive(Deserialize)]
pub struct AppealForm {
    kind: String,
    id: i64,
    message: String,
}

pub async fn appeal(State(st): State<AppState>, RequireUser(u): RequireUser, Form(f): Form<AppealForm>) -> AppResult<Response> {
    moderation::appeal(&st.db, st.clock.now(), &u, &f.kind, f.id, &f.message).await?;
    Ok(to("/me", "appealed"))
}

// ---- static / meta -----------------------------------------------------------------------------------

pub async fn rules(MaybeUser(u): MaybeUser) -> AppResult<Response> {
    let b = "<h1>Aturan</h1>\
    <h2>Struktur</h2><ul>\
    <li>Tiga channel universal setara: <b>#E</b>, <b>#Z</b>, <b>#D</b>. Channel asal dipilih saat daftar dan tidak bisa diganti.</li>\
    <li>Channel asal <b>hanya</b> mengunci dukungan ideologi (dan pendirian ideologi). Ruang diskusi #E, #Z, #D terbuka: siapa pun bebas membaca dan menulis di semuanya, seperti forum biasa.</li>\
    <li>Ideologi dibuat pengguna. Identitasnya hanya nama, warna gelembung channel, dan tulisan kecil di samping nama penulis.</li>\
    <li>Final Nine = 3 ideologi per channel. Itu status kompetitif, bukan jabatan; tidak memberi hak admin atau moderasi.</li></ul>\
    <h2>Dukungan</h2><ul>\
    <li>1 akun = 1 dukungan aktif, hanya untuk ideologi di channel sendiri.</li>\
    <li>Boleh pindah, tetapi minimal 7 hari sejak dukungan terakhir.</li>\
    <li>Selama minggu ke-4 election (sampai hasil ditetapkan) semua dukungan dikunci.</li>\
    <li>Angka dukungan = akun aktif yang mendukung. Ini bukan verifikasi manusia unik.</li></ul>\
    <h2>Siklus</h2><ul>\
    <li>Satu generasi = 6 bulan (WIB). Election 4 minggu terakhir menentukan generasi berikutnya, tanpa jeda.</li>\
    <li>Seleksi mingguan: 10, 7, 5, lalu 3 teratas per channel. Ideologi lama tidak dihapus saat turun dari Final Nine.</li>\
    <li>Kuota pendirian ideologi per channel per generasi = max(S_min, 10% pengguna aktif channel).</li></ul>\
    <h2>Perilaku</h2><ul>\
    <li>Dilarang: pelecehan, ancaman, doxxing, spam, manipulasi vote. Kritik teknis dan persaingan ideologi diperbolehkan.</li>\
    <li>Konten bisa dilaporkan; admin dapat menyembunyikan konten atau membatasi akun/ideologi. Semua tindakan tercatat di audit log.</li>\
    <li>Kamu dapat mengajukan banding dari halaman profil.</li></ul>";
    ok_html(page("Aturan", u.as_ref(), None, b))
}

pub async fn healthz(State(st): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&st.db).await {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "db").into_response(),
    }
}

pub async fn css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        include_str!("style.css"),
    )
        .into_response()
}

pub async fn not_found(MaybeUser(u): MaybeUser) -> Response {
    (StatusCode::NOT_FOUND, page("Tidak ditemukan", u.as_ref(), None, "<h1>404</h1><p>Halaman tidak ditemukan.</p>")).into_response()
}
