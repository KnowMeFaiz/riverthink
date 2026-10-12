# Benchmark

Perintah: `cargo run --release --example bench` (default 3000 pengguna, 300 ideologi, 5000 catatan).
Server dan klien beban berjalan **di proses dan CPU yang sama**, jadi angka server sebenarnya lebih baik daripada yang tertera.

## Lingkungan

- CPU: Intel Xeon @ 2,10 GHz, **2 core logis** (VM cloud), Linux
- Build: `--release`, LTO fat, `panic=abort`, `opt-level=2`
- Server: 2 worker tokio, SQLite WAL, pool 4 koneksi, `synchronous=NORMAL`
- Beban: loopback HTTP/1.1 keep-alive, 32 koneksi, 5 detik per skenario; vote: 64 task serentak

## Hasil (jalan terakhir)

| Pengukuran | Hasil |
|---|---|
| Vote serentak (3000 user, 64 in-flight) | 3000/3000 sukses, **±2.470 vote/dtk**, p50 ≈ 24 ms, p99 ≈ 40–52 ms, **0 baris vote ganda** |
| Hitung dukungan + aktivitas per channel (100 ideologi) | ≈ 3 ms |
| Finalisasi election genesis (4 seleksi, 300 ideologi) | ≈ 30–36 ms, tepat 9 baris Final Nine |
| `GET /` (20 catatan, 7,8 KB) | ≈ 2.600 req/dtk, p50 12 ms, p99 18 ms |
| `GET /ideologies` (300 ideologi, 45,7 KB, cache 5 dtk) | ≈ 15.450 req/dtk, p50 1,8 ms, p99 3,7 ms |
| `GET /final-nine` (2,7 KB) | ≈ 1.970 req/dtk, p50 16 ms |
| `GET /tag/bench` (5000 catatan ber-tag) | ≈ 1.350 req/dtk, p50 23 ms |
| Memori (RSS) | server idle ±6 MB; ±25–27 MB setelah semua beban (termasuk klien benchmark) |
| Ukuran binary | 3,3 MB |

## Temuan yang sudah ditindaklanjuti

Sebelum cache papan, `GET /ideologies` hanya **140 req/dtk** (p50 234 ms) karena tiap request menghitung ulang dukungan untuk semua ideologi. Dengan cache 5 detik menjadi ±15.450 req/dtk. Halaman detail dan aksi vote tidak di-cache.

## Cara membaca

Angka ini membuktikan kecukupan untuk MVP satu-instance pada skala beberapa ribu pengguna dengan data sebesar di atas, **bukan** jaminan skala produksi. `GET /` dan `/tag/*` paling berat (join catatan–pengguna–vote–ideologi); bila nanti menjadi penghambat, langkah berikutnya adalah menyimpan label ideologi penulis di tabel catatan atau cache seperti papan ideologi. Ulangi benchmark di perangkat target (laptop L380) dan catat hasilnya di sini.
