# RiverThink

Platform komunitas teknologi tempat ideologi buatan pengguna bersaing memperebutkan **Final Nine** (3 kursi per channel E / Z / D) dalam siklus generasi 6 bulan. MVP lokal: satu binary Rust (±3,3 MB), SQLite, HTML server-render tanpa JavaScript.

Dokumen: [`docs/DECISIONS.md`](docs/DECISIONS.md) (semua keputusan desain) · [`docs/DOMAIN.md`](docs/DOMAIN.md) (domain model, skema, siklus).

## Menjalankan

Butuh Rust ≥ 1.85 (diuji dengan 1.97). Tidak ada layanan lain; biaya IDR 0.

```bash
cargo run --release                 # http://127.0.0.1:3000, database riverthink.db dibuat otomatis
```

Buat akun lewat `/register`, lalu jadikan diri sendiri admin (satu-satunya jalur; tidak ada kenaikan peran dari web):

```bash
cargo run --release -- admin-grant NAMA_USER
```

Akses dari Android/laptop lain di jaringan yang sama: `RIVERTHINK_BIND=0.0.0.0:3000`. Untuk internet gunakan reverse proxy HTTPS (mis. Caddy) dan set `RIVERTHINK_COOKIE_SECURE=1` dan `RIVERTHINK_TRUST_PROXY=1`.

| Variabel | Default | Arti |
|---|---|---|
| `RIVERTHINK_DB` | `riverthink.db` | lokasi file SQLite |
| `RIVERTHINK_BIND` | `127.0.0.1:3000` | alamat dengar |
| `RIVERTHINK_SMIN` | `6` | `S_min` Rule A |
| `RIVERTHINK_DEV_CLOCK` | off | **demo saja**: admin bisa memajukan jam di `/admin` |
| `RIVERTHINK_PHONE_PEPPER` | `dev-pepper-change-me` | kunci hash nomor telepon (**wajib diganti** di produksi, jangan diubah setelah ada akun) |
| `RIVERTHINK_NO_PHONE` | off | demo/tes: pendaftaran tanpa nomor telepon |
| `RIVERTHINK_COOKIE_SECURE` | off | tambahkan `Secure` pada cookie (HTTPS) |
| `RIVERTHINK_TRUST_PROXY` | off | ambil IP klien dari `X-Forwarded-For` (hanya untuk rate limit) |

### Demo siklus penuh tanpa menunggu 6 bulan

```bash
RIVERTHINK_DEV_CLOCK=1 cargo run --release
```
Masuk sebagai admin → `/admin` → "Majukan jam". Maju 7 hari ×4 = seleksi 1–4 dan Final Nine pertama; maju ±180 hari lagi = election generasi 2 dan seterusnya. Jam demo hanya di memori (reset saat restart) dan lengkap dengan peringatan di log.

### Data simulasi (database terpisah)

```bash
$env:RIVERTHINK_DB="demo.db"; cargo run --release --example seed   # 36 akun sim, 9 ideologi, 27 vote (password demo12345)
$env:RIVERTHINK_DB="demo.db"; $env:RIVERTHINK_DEV_CLOCK="1"; cargo run --release
```

## Tes dan benchmark

```bash
cargo test                                   # 46 tes: 30 unit (domain murni) + 16 integrasi
cargo run --release --example bench          # benchmark: [users] [ideologi/channel] [catatan]
```

Hasil terukur ada di [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).

## Bukti acceptance (§26)

| # | Kriteria | Bukti |
|---|----------|-------|
| 1 | Publikasi dan baca Ambinote | `ambinote_tags_filtering_and_rate_limit`; uji HTTP |
| 2 | Navigasi tag dan ruang | `/tag/{tag}` dan ruang terbuka `/c/{E,Z,D}` (tes `channel_rooms_are_open_to_everyone...`). **Channel tematik/kustom ditunda** (D-01) |
| 3 | Tiga channel aturan setara | kode tanpa cabang per-channel; `genesis_election_runs_four_weekly_selections...` memakai aturan sama di E/Z/D |
| 4 | Buat ideologi dengan nama pilihan | `one_ideology_per_account_per_generation_and_unique_names` |
| 5 | Rule A | `capacity_*` (3 unit + 2 integrasi) |
| 6–7 | Dukungan sah; duplikat/invalid ditolak | `one_account_one_vote_with_cooldown`, `concurrent_votes_never_create_invalid_state` (180 permintaan serentak, tepat 30 menang) |
| 8 | Empat tahap mingguan | `genesis_election_runs_four_weekly_selections...` |
| 9 | Tepat 3 per channel | tes yang sama + constraint DB (`database_rejects_invalid_final_nine...`) |
| 10–11 | Ideologi bertahan lintas generasi; pendatang baru bisa naik | `incumbent_survives_or_is_replaced_across_generations_without_gaps` |
| 12 | Tak ada masa tanpa Final Nine | tes yang sama memeriksa **setiap hari** selama satu generasi penuh |
| 13 | Riwayat tersedia | `election::history` + halaman `/final-nine` |
| 14 | Integritas saat konkuren | 8 `tick` serentak = tepat satu finalisasi; vote serentak; benchmark 3000 vote serentak, 0 duplikat |
| 15 | Build, tes, jalan | bagian ini |

Verifikasi runtime (bukan hanya kompilasi): server release dijalankan dan diuji lewat HTTP nyata (daftar, login, posting, XSS ter-escape, CSRF 403, vote lintas-channel 403, duplikat 409, kunci minggu 4 409, siklus genesis → generasi 2 dengan jam demo).

## Batasan yang diketahui (jujur)

- **Nomor telepon belum diverifikasi OTP** (D-16): batas 1 akun/nomor hanya menghitung nomor yang diketik (OTP ditunda).
- **Akun palsu** adalah satu-satunya jalur manipulasi 1-akun-1-vote. Belum ada verifikasi email/manusia; hanya rate limit pendaftaran per IP (5/jam). Wajib ditambah verifikasi sebelum dibuka publik. Angka dukungan **bukan** jumlah manusia unik.
- Rate limiter ada di memori dan reset saat restart; IP di balik NAT berbagi kuota.
- SQLite: satu penulis pada satu waktu. Cukup untuk ribuan pengguna (lihat benchmark), tetapi hanya satu instance. Tidak ada replikasi; cadangkan file `.db` (aman disalin saat `sqlite3 .backup`, jangan salin mentah saat WAL aktif).
- Belum ada HTTPS bawaan, reset password, verifikasi email, notifikasi, atau unggah gambar.
- Papan `/ideologies` di-cache 5 detik (angka dukungan di daftar bisa tertinggal ≤5 dtk; halaman detail dan vote selalu live).
- "Aktif" = login/aktivitas dalam 30 hari; pembaruan `last_active_at` maksimal sekali per jam.
- Seleksi memakai dukungan **saat seleksi dijalankan**, bukan akumulasi lama-dukungan. Persistensi dukungan (§15) masuk ke ranah prestasi yang ditunda.
- Belum ada pagination di daftar ideologi (cukup sampai ratusan; tambahkan bila melebihi).
- Berjalan 100% lokal; tidak ada klaim uptime atau kapasitas produksi.

## Ditunda (D-01)

Channel tematik/kustom, prestasi dan kelayakan race, kuota per tema, moderasi AI, rekomendasi, aplikasi native.
