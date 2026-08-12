# DBManage

Multi-database management platform (MariaDB & PostgreSQL) berbasis Rust + Axum.

Lihat [PRD.md](./PRD.md) untuk spesifikasi lengkap.

## Menjalankan

```bash
# wajib: key untuk enkripsi password koneksi & session (min. 16 karakter)
export DBMANAGE_SECRET_KEY="ganti-dengan-string-acak-yang-panjang"

# opsional
export DBMANAGE_PORT=3000        # default 3000
export DBMANAGE_HOST=0.0.0.0     # default 0.0.0.0
export DBMANAGE_DATA_DIR=./data  # lokasi SQLite internal

cargo run --release
```

Buka `http://<ip>:3000` → otomatis redirect ke `/session_{random}` untuk login.
Saat pertama kali dijalankan, Anda akan dipandu membuat akun admin + aktivasi 2FA (TOTP).

## Build di CI

Build release dilakukan lewat GitHub Actions (`.github/workflows/build.yml`);
binary hasil build tersedia sebagai artifact `dbmanage-linux-x64`.

## Status Milestone

- [x] M1 — Core: session random URL, login + bcrypt + TOTP 2FA, rate limiting, audit log, dashboard skeleton
- [ ] M2 — Manajemen koneksi MariaDB & PostgreSQL
- [ ] M3 — Database explorer (struktur, ERD, data grid, CRUD)
- [ ] M4 — Autobackup S3 & Google Drive
- [ ] M5 — Polish
