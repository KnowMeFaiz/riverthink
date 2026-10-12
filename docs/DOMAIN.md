# Domain model dan arsitektur

## Lapisan (§19)

```
src/domain/    aturan murni: calendar, capacity (Rule A), channel, vote (kebijakan), election (seleksi)
               tanpa HTTP, SQL, atau jam sistem; semua waktu masuk lewat parameter
src/service/   orkestrasi aturan + SQL: auth, notes, ideology, vote, election (scheduler), moderation
src/db.rs      koneksi SQLite + `immediate()` = BEGIN IMMEDIATE untuk setiap tulis
src/web/       router Axum, extractor sesi, middleware (CSRF, header keamanan), halaman HTML, admin
migrations/    skema berversi (sqlx migrate), dijalankan otomatis saat start
tests/ examples/   integrasi dan benchmark
```

Waktu disuntikkan (`now`) ke semua service, jadi enam bulan disimulasikan dalam milidetik.
`web::AppState.clock` hanya menambah offset demo bila `RIVERTHINK_DEV_CLOCK=1`.

## Entitas

| Tabel | Isi | Invarian yang ditegakkan DB |
|-------|-----|-----------------------------|
| `users` | akun, channel asal, peran, status | username unik (NOCASE); channel ∈ {E,Z,D}; **channel immutable** (trigger); `phone_hash` maks 1 akun per nomor (trigger); `deleted_at` = akun dianonimkan |
| `sessions` | hash token, kedaluwarsa | hanya hash disimpan |
| `ideologies` | nama pilihan pendiri, deskripsi, channel, `founded_generation`, status | nama unik (NOCASE); channel & generasi pendiri immutable; `UNIQUE(id, channel)` |
| `votes` | **satu baris per user** (`user_id` PK), ideologi, `cast_at` | FK `(ideology_id, channel)`; channel vote = channel user (trigger) |
| `vote_log` | riwayat perpindahan dukungan | append |
| `notes`, `note_tags` | Ambinote + tag ternormalisasi (huruf kecil, ≤5/catatan); `room` opsional ∈ {E,Z,D} (NULL = Umum) | panjang dibatasi CHECK; `room` dibatasi CHECK |
| `elections` | satu per generasi target, `stage` 0..4, `running/finalized` | `target_generation` unik |
| `election_entries` | medan kandidat beku, `eliminated_stage` | PK (election, ideology) |
| `election_stage_results` | audit tiap seleksi: dukungan, peringkat, lolos | PK (election, stage, ideology) |
| `generations` | nomor, `starts_at`, `ends_at` | `ends_at > starts_at` |
| `final_nine` | `(generation, channel, slot 1..3) → ideology` | **slot 1..3 per channel = maks 3/channel dan 9 total secara struktural**; ideologi harus di channel yang sama (FK komposit); `UNIQUE(generation, ideology)`; **append-only** (trigger tolak UPDATE/DELETE) |
| `reports`, `appeals`, `audit_log` | moderasi | 1 laporan per (pelapor, target); 1 banding terbuka per target |

## Siklus hidup ideologi

`didirikan (gen pendiri = generasi terakhir yang final)` → ikut election berikutnya sebagai kandidat → seleksi 1..4 → **di Final Nine** atau **tetap ada di luar Final Nine** (identitas, pendukung, riwayat dipertahankan; tidak pernah dihapus) → bisa kembali merebut kursi di election mana pun. `restricted` oleh moderator = tidak bisa didukung dan gugur dari seleksi, bisa dipulihkan lewat banding.

## Siklus waktu

```
genesis: server start ── 4 minggu election ──► Gen 1 mulai (G1)
G1 ........................... G1+6 bln (G1 berakhir)
                       └ 4 minggu election Gen 2 ┘ ► Gen 2 mulai tepat di G1+6 bln
minggu ke-k selesai ⇒ seleksi k; minggu 4 selesai ⇒ finalisasi + generasi baru
vote dikunci mulai awal minggu 4 hingga finalisasi
```

Scheduler (`service::election::tick`) membaca "apa yang jatuh tempo", lalu menerapkannya satu langkah per transaksi; panggilan paralel menghasilkan tepat satu penerapan (diuji dengan 8 `tick` konkuren).

## Halaman

`/` Ambinote (semua ruang) · `/c/{E|Z|D}` ruang diskusi terbuka · `/tag/{tag}` · `/ideologies` (3 kolom E/Z/D, kuota Rule A) · `/ideologies/{id}` · `/final-nine` (+ riwayat) · `/election` · `/me` · `/rules` · `/login` `/register` · `/admin` · `/healthz`.
