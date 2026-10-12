use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use super::html::{esc, page_with};
use super::{AppState, RequireAdmin};
use crate::domain::calendar::{DAY, format_wib};
use crate::error::{AppError, AppResult};
use crate::service::{election, moderation};

#[derive(Deserialize, Default)]
pub struct Q {
    m: Option<String>,
}

fn back() -> Response {
    Redirect::to("/admin?m=done").into_response()
}

fn action_form(kind: &str, id: i64, action: &str, label: &str) -> String {
    format!(
        "<form class=\"inline\" method=\"post\" action=\"/admin/act\">\
         <input type=\"hidden\" name=\"kind\" value=\"{kind}\"><input type=\"hidden\" name=\"id\" value=\"{id}\">\
         <input type=\"hidden\" name=\"action\" value=\"{action}\"><input type=\"hidden\" name=\"note\" value=\"dari laporan\">\
         <button>{label}</button></form>"
    )
}

pub async fn overview(State(st): State<AppState>, RequireAdmin(a): RequireAdmin, Query(q): Query<Q>) -> AppResult<Response> {
    let o = moderation::admin_overview(&st.db).await?;
    let mut b = String::from("<h1>Admin</h1>");

    b.push_str(&format!("<h2>Laporan terbuka ({})</h2>", o.reports.len()));
    if o.reports.is_empty() {
        b.push_str("<p class=\"mut\">Tidak ada.</p>");
    }
    for r in &o.reports {
        let act = match r.target_kind.as_str() {
            "note" => action_form("note", r.target_id, "hide", "sembunyikan"),
            k => action_form(k, r.target_id, "restrict", "batasi"),
        };
        b.push_str(&format!(
            "<div class=\"card\"><b>#{id}</b> {k} <b>{l}</b> (id {tid}) — dilaporkan {rp} pada {t}<br>“{reason}”<br>{act} \
             <form class=\"inline\" method=\"post\" action=\"/admin/report/{id}/dismiss\"><button>abaikan</button></form></div>",
            id = r.id,
            k = esc(&r.target_kind),
            l = esc(r.label.as_deref().unwrap_or("?")),
            tid = r.target_id,
            rp = esc(&r.reporter),
            t = format_wib(r.created_at),
            reason = esc(&r.reason)
        ));
    }

    b.push_str(&format!("<h2>Banding terbuka ({})</h2>", o.appeals.len()));
    if o.appeals.is_empty() {
        b.push_str("<p class=\"mut\">Tidak ada.</p>");
    }
    for p in &o.appeals {
        b.push_str(&format!(
            "<div class=\"card\"><b>#{id}</b> {u} membanding {k} <b>{l}</b> pada {t}<br>“{m}”\
             <form method=\"post\" action=\"/admin/appeal/{id}/grant\"><input type=\"text\" name=\"note\" placeholder=\"catatan admin\" maxlength=\"500\"><button>kabulkan</button></form>\
             <form method=\"post\" action=\"/admin/appeal/{id}/deny\"><input type=\"text\" name=\"note\" placeholder=\"catatan admin\" maxlength=\"500\"><button class=\"alt\">tolak</button></form></div>",
            id = p.id,
            u = esc(&p.username),
            k = esc(&p.target_kind),
            l = esc(p.label.as_deref().unwrap_or("?")),
            t = format_wib(p.created_at),
            m = esc(&p.message)
        ));
    }

    b.push_str(
        "<h2>Tindakan manual</h2><form class=\"card\" method=\"post\" action=\"/admin/act\">\
         <select name=\"kind\"><option value=\"note\">catatan</option><option value=\"user\">user</option><option value=\"ideology\">ideologi</option></select>\
         <input type=\"number\" name=\"id\" placeholder=\"id target\" required>\
         <select name=\"action\"><option>hide</option><option>unhide</option><option>restrict</option><option>unrestrict</option></select>\
         <input type=\"text\" name=\"note\" placeholder=\"catatan (masuk audit log)\" maxlength=\"500\">\
         <button>Jalankan</button></form>",
    );

    if st.cfg.dev_clock {
        b.push_str(&format!(
            "<h2>Jam demo</h2><p class=\"mut\">RIVERTHINK_DEV_CLOCK aktif. Offset saat ini: {} hari. Jangan aktifkan di produksi.</p>\
             <form class=\"card\" method=\"post\" action=\"/admin/dev/advance\"><input type=\"number\" name=\"days\" value=\"7\" min=\"1\" max=\"400\">\
             <button>Majukan jam (hari)</button></form>",
            st.clock.offset() / DAY
        ));
    }

    b.push_str("<h2>Audit log (50 terakhir)</h2><table><tr><th>Waktu</th><th>Aktor</th><th>Aksi</th><th>Target</th><th>Catatan</th></tr>");
    for l in &o.audit {
        b.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{} {}</td><td>{}</td></tr>",
            format_wib(l.at),
            esc(l.actor.as_deref().unwrap_or("sistem")),
            esc(&l.action),
            esc(&l.target_kind),
            l.target_id,
            esc(&l.detail)
        ));
    }
    b.push_str("</table>");
    Ok(page_with("Admin", Some(&a), q.m.as_deref(), &b, true).into_response())
}

#[derive(Deserialize)]
pub struct ActForm {
    kind: String,
    id: i64,
    action: String,
    #[serde(default)]
    note: String,
}

pub async fn act(State(st): State<AppState>, RequireAdmin(a): RequireAdmin, Form(f): Form<ActForm>) -> AppResult<Response> {
    moderation::act(&st.db, st.clock.now(), &a, &f.kind, f.id, &f.action, &f.note).await?;
    Ok(back())
}

#[derive(Deserialize, Default)]
pub struct NoteForm {
    #[serde(default)]
    note: String,
}

pub async fn dismiss(State(st): State<AppState>, RequireAdmin(a): RequireAdmin, Path(id): Path<i64>) -> AppResult<Response> {
    moderation::dismiss_report(&st.db, st.clock.now(), &a, id, "").await?;
    Ok(back())
}

pub async fn resolve_appeal(
    State(st): State<AppState>,
    RequireAdmin(a): RequireAdmin,
    Path((id, verdict)): Path<(i64, String)>,
    Form(f): Form<NoteForm>,
) -> AppResult<Response> {
    let grant = match verdict.as_str() {
        "grant" => true,
        "deny" => false,
        _ => return Err(AppError::NotFound),
    };
    moderation::resolve_appeal(&st.db, st.clock.now(), &a, id, grant, &f.note).await?;
    Ok(back())
}

#[derive(Deserialize)]
pub struct AdvanceForm {
    days: i64,
}

/// Demo only: fast-forward the application clock and run the scheduler immediately.
pub async fn dev_advance(State(st): State<AppState>, RequireAdmin(_a): RequireAdmin, Form(f): Form<AdvanceForm>) -> AppResult<Response> {
    if !st.cfg.dev_clock {
        return Err(AppError::NotFound);
    }
    if !(1..=400).contains(&f.days) {
        return Err(AppError::Invalid("Hari harus 1-400.".into()));
    }
    st.clock.advance(f.days * DAY);
    // Demo only: keep every login alive across the jump so the admin is not logged out.
    sqlx::query("UPDATE sessions SET expires_at = expires_at + ?1").bind(f.days * DAY).execute(&st.db).await?;
    election::tick(&st.db, st.clock.now()).await?;
    Ok(back())
}
