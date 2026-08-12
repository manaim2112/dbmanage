# PRD — DBManage: Multi-Database Management Platform

## 1. Ringkasan Eksekutif

DBManage adalah aplikasi web berbasis Rust untuk mengelola banyak server database (MariaDB & PostgreSQL) dari satu antarmuka terpadu. Aplikasi diakses melalui `ip:port`, lalu dialihkan ke `/session_{random}` untuk login dengan 2FA. Di dalamnya: kelola koneksi database jarak jauh, buat user dengan password otomatis, browsing tabel & data, serta autobackup ke S3 / Google Drive.

---

## 2. Tujuan

| # | Tujuan | Ukuran Keberhasilan |
|---|--------|---------------------|
| 1 | Akses aman via session random + 2FA | Tidak ada akses tanpa session valid & TOTP |
| 2 | Kelola banyak koneksi MariaDB & PostgreSQL | Tambah/hapus/test koneksi tanpa restart |
| 3 | Manajemen user database | Buat user + password kuat otomatis dalam 1 klik |
| 4 | Browsing database: tabel, data, struktur | Tabel tampil < 500ms, data paginasi |
| 5 | Autobackup ke S3 & Google Drive | Backup terjadwal harian + manual, retry gagal |

---

## 3. Tech Stack

| Lapisan | Teknologi | Alasan |
|---------|-----------|--------|
| Web framework | **Axum** | Paling cepat di ekosistem Rust, berbasis tokio/hyper |
| Async runtime | Tokio | Standar de-facto, diperlukan Axum |
| Database driver | **SQLx** (async) | Satu crate untuk MariaDB + PostgreSQL, compile-time query check |
| Internal DB | SQLite (via SQLx) | Simpan session, user, konfigurasi — ringan, tanpa server |
| Templating | **Askama** | Compile-time, zero-runtime overhead, aman XSS |
| Frontend | HTMX + Alpine.js + Tailwind CSS | Interaktif tanpa SPA framework, ringan, reaktif |
| ERD / Diagram | **mermaid.js** | Render diagram relasi tabel (erDiagram) di browser |
| Ikon | Lucide Icons | Ikon konsisten untuk PK/FK/type/aksi |
| 2FA | `totp-rs` | TOTP standard (RFC 6238), kompatibel Google Authenticator |
| S3 | `aws-sdk-s3` | Official AWS SDK untuk Rust |
| Google Drive | `google-drive3` + `oauth2` | REST API Google Drive |
| Password gen | Custom (`rand` + `passwords`) | Generate password kuat 24-char |
| Container | Docker (opsional) | Deploy konsisten |

---

## 4. Arsitektur Sistem

```
┌─────────────────────────────────────────────────┐
│                   Browser                        │
│         ip:port → /session_{random}              │
└─────────────────┬───────────────────────────────┘
                  │ HTTPS (production) / HTTP (dev)
┌─────────────────▼───────────────────────────────┐
│              Axum Server (Rust)                  │
│                                                  │
│  ┌──────────┐  ┌──────────┐  ┌───────────────┐  │
│  │ Session  │  │   Auth   │  │  2FA (TOTP)   │  │
│  │ Middleware│  │ Controller│  │  Service      │  │
│  └──────────┘  └──────────┘  └───────────────┘  │
│                                                  │
│  ┌──────────┐  ┌──────────┐  ┌───────────────┐  │
│  │Connection│  │  Query   │  │   Backup      │  │
│  │ Manager  │  │  Engine  │  │   Scheduler   │  │
│  └──────────┘  └──────────┘  └───────────────┘  │
│                                                  │
│  ┌──────────────────────────────────────────┐    │
│  │         SQLite (internal state)           │    │
│  │  • sessions  • users  • connections       │    │
│  │  • backup_configs  • backup_history       │    │
│  └──────────────────────────────────────────┘    │
└──────────────────┬──────────────────────────────┘
                   │
    ┌──────────────┼──────────────┐
    ▼              ▼              ▼
┌────────┐  ┌──────────┐  ┌──────────┐
│MariaDB │  │PostgreSQL│  │  ...N    │
│Server 1│  │Server 1  │  │          │
└────────┘  └──────────┘  └──────────┘
```

### 4.1 Alur Session

1. User buka `http://ip:port`
2. Server generate `session_id` random (32-char hex)
3. Redirect ke `/session_{session_id}`
4. Session tersimpan di SQLite dengan status `pending`
5. User login (username + password + TOTP)
6. Session status berubah jadi `authenticated`
7. Semua request berikutnya diperiksa session cookie + session_id di path

### 4.2 Alur 2FA Setup

1. Admin pertama kali setup → generate TOTP secret
2. Tampilkan QR code (via `qrcode` crate atau JS library)
3. User scan dengan Google Authenticator / Authy
4. Verifikasi dengan kode TOTP saat login

---

## 5. Fitur Detail

### 5.1 Session & Autentikasi

| Fitur | Deskripsi |
|-------|-----------|
| Random session URL | `/session_{32-char-hex}` — unik per browser, expire 24 jam idle |
| Login | Username + password (bcrypt hashed) |
| 2FA TOTP | Wajib saat login, 6-digit code, 30-detik window |
| Session timeout | 24 jam idle → auto logout |
| Brute-force protection | Rate limiting: 5 gagal → lock 15 menit |

### 5.2 Manajemen Koneksi Database

| Fitur | Deskripsi |
|-------|-----------|
| Tambah koneksi | Form: nama, host, port, tipe (MariaDB/PostgreSQL), user, password |
| Test koneksi | Tombol "Test" — verifikasi koneksi berhasil |
| Edit koneksi | Ubah parameter koneksi yang sudah ada |
| Hapus koneksi | Konfirmasi sebelum hapus |
| Connection pool | Setiap koneksi punya pool sendiri (min 2, max 10) |
| Status indikator | Hijau (online) / Merah (offline) |

### 5.3 Manajemen Database & User

| Fitur | Deskripsi |
|-------|-----------|
| List database | Tampilkan semua database dalam server |
| Buat database | Form: nama database, charset, collation |
| Hapus database | Konfirmasi + ketik nama database |
| Buat user | Auto-generate password 24-char (upper, lower, digit, symbol) |
| Grant privileges | Pilih database + privileges (SELECT, INSERT, ALL, dll) |
| List user | Tampilkan semua user dalam server |
| Hapus user | Konfirmasi sebelum hapus |
| Reset password | Generate password baru untuk user yang ada |

### 5.4 Database Explorer (Browsing)

#### 5.4.1 List Tabel
| Fitur | Deskripsi |
|-------|-----------|
| Tabel list | Nama, engine, row count, size (data+index), collation, jumlah kolom, jumlah FK |
| Pencarian cepat | Filter-as-you-type nama tabel (Ctrl+K di sidebar) |
| Grup per prefix | Opsi grouping otomatis `prefix_` (mis. `auth_users`, `auth_roles`) |
| Ringkasan DB | Total tabel, total rows, total size di header |

#### 5.4.2 Struktur Tabel
| Fitur | Deskripsi |
|-------|-----------|
| Kolom | Nama, tipe data + length/precision (mis. `varchar(255)`, `decimal(10,2)`), nullable, default, key (PK/FK/UQ), auto increment, comment |
| Badge key | Ikon per constraint: 🔑 PK, 🔗 FK, ⬡ UQ, ▤ IDX — klik FK langsung jump ke tabel target |
| Index list | Nama index, tipe (BTREE/HASH/FULLTEXT), unik/tidak, kolom yang dicover, urutan |
| Foreign key list | Nama FK, kolom lokal → `tabel_target.kolom_target`, aksi `ON DELETE` / `ON UPDATE` (CASCADE, SET NULL, RESTRICT, dll) |
| Preview DDL | Tombol "Lihat DDL" — tampilkan `CREATE TABLE` lengkap |
| Statistik | Row count, avg row length, data size, index size, last update |

#### 5.4.3 Relasi / ERD
| Fitur | Deskripsi |
|-------|-----------|
| Diagram ERD | Render mermaid `erDiagram` per database dari metadata FK |
| Scope | Pilih: seluruh database, atau radius N dari satu tabel (tabel + tetangganya) |
| Interaksi | Klik nama tabel di diagram → buka struktur tabel |
| Legend | Kardinalitas (1-N, 1-1, N-N via junction) ditampilkan dengan notasi crow's foot |
| Ekspor | Download diagram sebagai SVG/PNG |

#### 5.4.4 Lihat Data (Data Grid)
| Fitur | Deskripsi |
|-------|-----------|
| Paginasi | 50/100/200/500 rows per halaman, lazy load |
| Header kolom | Nama + tipe data di bawahnya (mis. `created_at` / `timestamp`) + ikon key |
| Render type-aware | `datetime` → format lokal + relative time tooltip; `boolean` → badge ✓/✗; `NULL` → badge abu "NULL"; `JSON` → preview truncated, klik untuk pretty-print modal; `BLOB` → badge ukuran byte; `decimal` → rata kanan dengan pemisah ribuan |
| Sort | Klik header untuk ASC/DESC, multi-kolom dengan Shift+klik |
| Filter | Bar filter per kolom: operator (=, !=, LIKE, >, <, IS NULL, IN) — digenerate jadi WHERE |
| Navigasi FK | Nilai kolom FK bisa diklik → jump langsung ke row terkait di tabel target |
| Ekspor | CSV / JSON / SQL INSERT dari hasil terfilter |
| Freeze kolom PK | Kolom kunci tetap terlihat saat scroll horizontal |
| Edit inline | Klik 2x cell → edit sesuai tipe data (date picker, JSON editor, checkbox) → save dengan konfirmasi |
| Insert row | Tombol "+ Row" → form field sesuai skema (default, nullable dihormati) |
| Delete row | Pilih row → hapus dengan konfirmasi (tampilkan nilai PK yang akan dihapus) |
| Bulk action | Multi-select row untuk delete massal (konfirmasi eksplisit + jumlah row) |

#### 5.4.5 Detail Row
| Fitur | Deskripsi |
|-------|-----------|
| Panel samping | Klik row → drawer kanan dengan semua kolom sebagai key-value vertikal, bisa diedit |
| Copy value | Tombol copy per nilai |
| Relasi row | Daftar "rows terkait" dari tabel lain yang menunjuk ke row ini (reverse FK lookup) |
| Hapus row | Tombol hapus dengan konfirmasi dari drawer |

#### 5.4.6 SQL Editor
| Fitur | Deskripsi |
|-------|-----------|
| Editor | Textarea dengan syntax highlighting (CodeMirror 6) |
| Autocomplete | Saran nama tabel & kolom dari skema aktif |
| Read-only | Default hanya SELECT; statement write butuh toggle eksplisit + konfirmasi |
| Hasil | Grid hasil + execution time + jumlah row terdampak |

### 5.5 Autobackup

| Fitur | Deskripsi |
|-------|-----------|
| Konfigurasi S3 | Endpoint, bucket, region, access key, secret key, path prefix |
| Konfigurasi Google Drive | OAuth2 flow, folder ID |
| Jadwal backup | Per koneksi: pilih frekuensi (harian/mingguan), jam, database yang dibackup |
| Backup manual | Tombol "Backup Sekarang" |
| Format backup | `mysqldump` / `pg_dump` — SQL dump, dikompresi gzip |
| History | Daftar backup: timestamp, ukuran, status, lokasi |
| Retensi | Konfigurasi hapus backup > N hari |

---

## 6. Skema Database Internal (SQLite)

### 6.1 `sessions`
```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,           -- 32-char hex
    created_at INTEGER NOT NULL,   -- unix timestamp
    last_active INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',  -- pending | authenticated | expired
    user_id INTEGER REFERENCES users(id)
);
```

### 6.2 `users`
```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,   -- bcrypt
    totp_secret TEXT,              -- TOTP secret (base32)
    totp_enabled INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
```

### 6.3 `connections`
```sql
CREATE TABLE connections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    db_type TEXT NOT NULL CHECK(db_type IN ('mariadb', 'postgresql')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    username TEXT NOT NULL,
    password_encrypted TEXT NOT NULL,  -- AES-256-GCM encrypted
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

### 6.4 `backup_configs`
```sql
CREATE TABLE backup_configs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    connection_id INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    database_name TEXT NOT NULL,
    provider TEXT NOT NULL CHECK(provider IN ('s3', 'gdrive')),
    config_json TEXT NOT NULL,     -- JSON: S3 creds atau GDrive token
    schedule_cron TEXT,            -- cron expression
    retention_days INTEGER NOT NULL DEFAULT 30,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);
```

### 6.5 `backup_history`
```sql
CREATE TABLE backup_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    backup_config_id INTEGER REFERENCES backup_configs(id) ON DELETE SET NULL,
    filename TEXT NOT NULL,
    size_bytes INTEGER,
    status TEXT NOT NULL CHECK(status IN ('running', 'success', 'failed')),
    provider TEXT NOT NULL,
    remote_path TEXT,
    error_message TEXT,
    started_at INTEGER NOT NULL,
    finished_at INTEGER
);
```

### 6.6 `audit_log`
```sql
CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    connection_id INTEGER REFERENCES connections(id) ON DELETE SET NULL,
    action TEXT NOT NULL,          -- grid_edit | grid_insert | grid_delete | sql_write | login | dll
    detail TEXT NOT NULL,          -- ringkasan: tabel, PK, statement
    executed_at INTEGER NOT NULL
);
```

---

## 7. Struktur Route

```
GET  /                          → redirect ke /session_{new_id}
GET  /session_{session_id}      → halaman login / dashboard
POST /session_{session_id}/login → proses login
POST /session_{session_id}/logout → logout

GET  /session_{session_id}/connections          → list koneksi
POST /session_{session_id}/connections          → tambah koneksi
POST /session_{session_id}/connections/{id}/test → test koneksi
PUT  /session_{session_id}/connections/{id}     → edit koneksi
DELETE /session_{session_id}/connections/{id}   → hapus koneksi

GET  /session_{session_id}/connections/{id}/databases     → list database
POST /session_{session_id}/connections/{id}/databases     → buat database
DELETE /session_{session_id}/connections/{id}/databases/{db} → hapus database

GET  /session_{session_id}/connections/{id}/users         → list user
POST /session_{session_id}/connections/{id}/users         → buat user + password
DELETE /session_{session_id}/connections/{id}/users/{user} → hapus user

GET  /session_{session_id}/connections/{id}/databases/{db}/tables       → list tabel
GET  /session_{session_id}/connections/{id}/databases/{db}/tables/{tbl} → struktur (kolom + index + FK)
GET  /session_{session_id}/connections/{id}/databases/{db}/tables/{tbl}/rows → data (paginated, type-aware)
GET  /session_{session_id}/connections/{id}/databases/{db}/tables/{tbl}/rows/{pk} → detail row + reverse FK
PUT  /session_{session_id}/connections/{id}/databases/{db}/tables/{tbl}/rows/{pk} → edit row (konfirmasi di UI)
POST /session_{session_id}/connections/{id}/databases/{db}/tables/{tbl}/rows → insert row baru
DELETE /session_{session_id}/connections/{id}/databases/{db}/tables/{tbl}/rows → hapus row (single/bulk)
GET  /session_{session_id}/connections/{id}/databases/{db}/tables/{tbl}/ddl → CREATE TABLE
GET  /session_{session_id}/connections/{id}/databases/{db}/erd          → metadata ERD (JSON, dirender mermaid)
POST /session_{session_id}/connections/{id}/query                        → SQL query (default read-only)

GET  /session_{session_id}/backups              → list konfigurasi backup
POST /session_{session_id}/backups              → tambah konfigurasi backup
POST /session_{session_id}/backups/{id}/run     → trigger backup manual
GET  /session_{session_id}/backups/{id}/history → history backup

GET  /session_{session_id}/settings             → pengaturan (ubah password, 2FA setup)
```

---

## 8. UI/UX — Design System & Wireframe

### 8.0 Prinsip Desain

1. **Information density tinggi tapi rapi** — tool untuk DBA/developer; data harus terlihat jelas tanpa banyak klik.
2. **Type-aware rendering** — setiap nilai ditampilkan sesuai tipe datanya (tanggal diformat, JSON bisa di-expand, NULL jelas terlihat).
3. **Semua metadata satu layar** — kolom, index, dan FK terlihat sekaligus di halaman struktur tabel.
4. **Navigasi via relasi** — FK adalah link; klik nilai FK atau badge FK langsung jump ke tabel/row target.
5. **Dark-first** — tema gelap default, light mode opsional.
6. **Keyboard-driven** — Ctrl+K cari tabel, Ctrl+Enter jalankan query, Esc tutup drawer/modal.

#### Design Tokens

| Token | Nilai |
|-------|-------|
| Font UI | Inter (system fallback) |
| Font data/kode | JetBrains Mono / ui-monospace |
| Warna base (dark) | `#0B0E14` bg, `#11151F` panel, `#1A2030` border |
| Aksen | Indigo `#6366F1` (aksi utama), Hijau `#10B981` (sukses/online), Merah `#EF4444` (error/offline), Amber `#F59E0B` (warning) |

#### Color Coding Tipe Data

| Kategori | Tipe | Warna badge |
|----------|------|-------------|
| Integer | int, bigint, smallint, serial | Biru |
| Decimal/Float | decimal, float, double, numeric | Biru terang |
| String | varchar, text, char | Hijau |
| Date/Time | date, datetime, timestamp, time | Ungu |
| Boolean | bool, tinyint(1) | Kuning |
| JSON | json, jsonb | Oranye |
| Binary | blob, bytea, binary | Abu-abu |
| UUID | uuid | Pink |

#### Ikon Constraint

| Constraint | Ikon | Keterangan |
|-----------|------|------------|
| Primary Key | 🔑 | Amber |
| Foreign Key | 🔗 | Biru, clickable → tabel target |
| Unique | ⬡ | Ungu |
| Index | ▤ | Abu |
| NOT NULL | ● | Dot merah kecil di samping nama |
| Auto Increment | ↻ | Abu |

### 8.1 Halaman Login
```
┌──────────────────────────────────┐
│          🔐 DBManage             │
│                                  │
│   ┌──────────────────────────┐   │
│   │  Username                │   │
│   └──────────────────────────┘   │
│   ┌──────────────────────────┐   │
│   │  Password                │   │
│   └──────────────────────────┘   │
│   ┌──────────────────────────┐   │
│   │  TOTP Code (6 digit)     │   │
│   └──────────────────────────┘   │
│                                  │
│   [        Login        ]        │
└──────────────────────────────────┘
```

### 8.2 Dashboard

```
┌────────────────────────────────────────────────────────────┐
│ ◈ DBManage    Dashboard  Connections  Backups    ⚙ admin ▾ │
├────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ 5 Koneksi   │  │ 12 Database │  │ 3 Backup    │         │
│  │ ● 4 online  │  │ 842 tabel   │  │ ● last OK   │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
│                                                            │
│  Koneksi                                        [+ Tambah] │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ ● Prod-MariaDB   mariadb 10.6.18   4 db   12.4 GB  > │  │
│  │ ● Staging-PG     postgres 16.4     2 db    3.1 GB  > │  │
│  │ ○ Dev-MariaDB    offline           —               > │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  Backup Terakhir                                           │
│  ✓ prod_myapp  2026-08-12 03:00  812 MB  s3://db-backups   │
└────────────────────────────────────────────────────────────┘
```

### 8.3 Database Explorer — List Tabel

Sidebar kiri persist di seluruh halaman explorer (connection → database → table).

```
┌──────────────┬─────────────────────────────────────────────┐
│ Prod-MariaDB │ Prod-MariaDB / myapp_db         14 tabel    │
│ ▾ myapp_db   │ total 2.4 jt rows • 4.2 GB     [ERD] [SQL]  │
│   📄 users   ├─────────────────────────────────────────────┤
│   📄 orders  │ 🔍 cari tabel… (Ctrl+K)                     │
│   📄 products├────────────┬───────┬───────┬──────┬─────────┤
│   📄 payments│ Tabel      │Engine │ Rows  │ Size │ FK      │
│   📄 reviews ├────────────┼───────┼───────┼──────┼─────────┤
│ ▸ analytics  │ users      │InnoDB │ 12.4k │ 8 MB │ —       │
│ ▸ auth       │ orders     │InnoDB │ 98.2k │64 MB │ 2 🔗    │
│              │ products   │InnoDB │  3.1k │ 2 MB │ 1 🔗    │
│ [+ DB][+User]│ payments   │InnoDB │ 97.9k │58 MB │ 2 🔗    │
└──────────────┴─────────────────────────────────────────────┘
```

### 8.4 Struktur Tabel

Tab per tabel: **[Struktur] [Data] [Relasi] [DDL]**

```
┌────────────────────────────────────────────────────────────┐
│ orders                                  98,241 rows • 64 MB │
│ [Struktur●] [Data] [Relasi] [DDL]                          │
├────────────────────────────────────────────────────────────┤
│ Kolom                                                      │
│ ┌───┬────────────┬───────────────┬──────┬────────┬───────┐ │
│ │ # │ Nama       │ Tipe          │ Null │Default │ Key   │ │
│ ├───┼────────────┼───────────────┼──────┼────────┼───────┤ │
│ │ 1 │ id         │ bigint(20) ↻  │  ✗   │ —      │ 🔑    │ │
│ │ 2 │ user_id    │ bigint(20) ●  │  ✗   │ —      │ 🔗 →  │ │
│ │   │            │               │      │        │ users │ │
│ │ 3 │ status     │ enum(4 nilai) │  ✗   │ 'new'  │ ▤     │ │
│ │ 4 │ total      │ decimal(10,2) │  ✗   │ 0.00   │       │ │
│ │ 5 │ note       │ text          │  ✓   │ NULL   │       │ │
│ │ 6 │ created_at │ timestamp     │  ✗   │ NOW()  │ ▤     │ │
│ └───┴────────────┴───────────────┴──────┴────────┴───────┘ │
│                                                            │
│ Foreign Keys                                               │
│ ┌────────────────┬─────────────────────┬──────────────────┐│
│ │ fk_orders_user │ user_id → users.id  │ DEL: CASCADE     ││
│ │                │                     │ UPD: CASCADE     ││
│ └────────────────┴─────────────────────┴──────────────────┘│
│                                                            │
│ Indexes                                                    │
│  PRIMARY (BTREE, unique)        → id                       │
│  idx_orders_status (BTREE)      → status, created_at       │
└────────────────────────────────────────────────────────────┘
```

### 8.5 Relasi / ERD (tab "Relasi")

```
┌────────────────────────────────────────────────────────────┐
│ Scope: [Seluruh DB ▾]   radius: [1 ▾]      [⬇ SVG][⬇ PNG]  │
├────────────────────────────────────────────────────────────┤
│                                                            │
│   ┌───────────┐        ┌───────────┐                       │
│   │  users    │1      N│  orders   │                       │
│   │───────────│────────│───────────│                       │
│   │ 🔑 id     │        │ 🔑 id     │                       │
│   │ email     │        │ 🔗 user_id│                       │
│   └───────────┘        └─────┬─────┘                       │
│                              │1                            │
│                              │N                            │
│                        ┌─────┴─────┐      ┌───────────┐    │
│                        │order_items│N    1│ products  │    │
│                        │───────────│──────│───────────│    │
│                        │ 🔑 id     │      │ 🔑 id     │    │
│                        │ 🔗 order  │      │ name      │    │
│                        │ 🔗 product│      │ price     │    │
│                        └───────────┘      └───────────┘    │
│                                                            │
│ Klik tabel di diagram → buka struktur/data tabel tsb       │
└────────────────────────────────────────────────────────────┘
```

### 8.6 Data Grid (tab "Data")

```
┌────────────────────────────────────────────────────────────┐
│ Filter: [kolom ▾][operator ▾][nilai…        ] [+ Filter]   │
│          [ Ekspor CSV ▾ ]                   98,241 rows     │
├────┬────────────┬───────────────┬────────────┬─────────────┤
│ id │ user_id    │ status        │ total      │ created_at  │
│bigint│bigint 🔗 │ enum          │ decimal    │ timestamp   │
├────┼────────────┼───────────────┼────────────┼─────────────┤
│ 1  │ 42 →       │ ● PAID        │   1,250.00 │ 12 Ags 2026 │
│ 2  │ 17 →       │ ● PENDING     │     340.50 │ 11 Ags 2026 │
│ 3  │ NULL       │ ● CANCELLED   │      99.00 │ 10 Ags 2026 │
├────┴────────────┴───────────────┴────────────┴─────────────┤
│ ◀ 1–50 of 98,241 ▶    rows/page: [50 ▾]    query 12ms     │
└────────────────────────────────────────────────────────────┘
  • Nilai FK (42, 17) = link → klik buka row tsb di `users`
  • Klik row mana pun → drawer detail (8.7)
```

### 8.7 Row Detail Drawer

```
┌────────────────────────────────────┐
│ Row: orders #1042        [copy][✕] │
├────────────────────────────────────┤
│ id          1042                   │
│ user_id     42 → [buka row]        │
│ status      ● PAID                 │
│ total       1,250.00               │
│ note        NULL                   │
│ created_at  2026-08-12 09:14:02    │
│             (2 jam lalu)           │
├────────────────────────────────────┤
│ Terkait (reverse FK):              │
│  • order_items: 3 rows →           │
│  • payments:    1 row  →           │
└────────────────────────────────────┘
```

---

## 9. Keamanan

| Area | Implementasi |
|------|-------------|
| Transport | HTTPS (production), redirect HTTP → HTTPS |
| Password user app | bcrypt, cost factor 12 |
| Password database | AES-256-GCM, encryption key dari environment variable |
| 2FA | TOTP (RFC 6238), 6-digit, 30-detik window |
| Session | Random 32-char hex, httpOnly cookie, 24 jam idle timeout |
| Rate limiting | 5 login gagal → lock 15 menit (per IP) |
| SQL injection | SQLx prepared statements (compile-time checked) |
| XSS | Askama auto-escaping + CSP header |
| CSRF | Token CSRF per session |
| Backup credentials | Encrypted at rest (AES-256-GCM) |
| Audit log | Semua operasi write (grid & SQL) dicatat di `audit_log` dengan timestamp |

---

## 10. Deployment

### 10.1 Binary Release
```bash
# Build optimized binary
cargo build --release

# Run
DBMANAGE_SECRET_KEY=... ./target/release/dbmanage
```

### 10.2 Docker
```dockerfile
FROM rust:1.80 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    mariadb-client postgresql-client \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/dbmanage /usr/local/bin/
EXPOSE 3000
CMD ["dbmanage"]
```

### 10.3 Environment Variables

| Variable | Wajib | Deskripsi |
|----------|-------|-----------|
| `DBMANAGE_SECRET_KEY` | Ya | 256-bit key untuk enkripsi password & session |
| `DBMANAGE_PORT` | Tidak | Default 3000 |
| `DBMANAGE_HOST` | Tidak | Default 0.0.0.0 |
| `DBMANAGE_DATA_DIR` | Tidak | Default `./data` (SQLite DB) |
| `TZ` | Tidak | Timezone untuk backup scheduler |

---

## 11. Milestone

### M1 — Core (Week 1-2)
- [x] Project setup: Axum + SQLite + Askama + Tailwind
- [x] Session generation & redirect (`/session_{random}`)
- [x] Login page + password auth (bcrypt)
- [x] TOTP 2FA setup & verification
- [x] Session middleware (cookie + path check)
- [x] Dashboard skeleton

### M2 — Connection Management (Week 3)
- [x] CRUD koneksi MariaDB & PostgreSQL
- [x] Test koneksi
- [x] Connection pooling
- [x] Status indikator (online/offline)

### M3 — Database Operations & Explorer (Week 4-5)
- [x] List/buat/hapus database
- [x] List/buat/hapus user + auto password
- [x] Grant privileges
- [x] List tabel dengan statistik (engine, rows, size, FK count)
- [x] Struktur tabel: kolom, tipe, key, index, foreign key, DDL
- [x] ERD: diagram relasi via mermaid (scope database)
- [x] Data grid type-aware: paginasi, sort, filter per kolom
- [x] Navigasi FK: link ke tabel referensi via tab Relasi
- [x] Reverse FK lookup (tab Relasi); row detail via edit inline
- [x] CRUD data: edit inline, insert row, delete dengan konfirmasi (bulk ditunda)
- [x] Audit log untuk semua operasi write
- [x] SQL editor (default read-only, toggle write tercatat di audit log)

### M4 — Backup (Week 6)
- [x] Konfigurasi S3
- [x] Konfigurasi Google Drive (OAuth2)
- [x] Backup manual
- [x] Backup scheduler (cron-based)
- [x] History & retensi

### M5 — Polish (Week 7)
- [x] Rate limiting
- [x] Ekspor CSV/JSON
- [x] Docker image
- [x] Dokumentasi

---

## 12. Keputusan (Decisions Log)

| # | Pertanyaan | Keputusan | Tanggal |
|---|-----------|-----------|---------|
| 1 | Write operations | ✅ **CRUD penuh + konfirmasi** — grid bisa edit/insert/delete (single + bulk), SQL editor boleh write via toggle eksplisit. Semua write dicatat di `audit_log`. | 2026-08-12 |
| 2 | Multi-user | ✅ **Single admin** — satu akun dengan 2FA. Skema SQLite tetap mendukung multi-user jika dibutuhkan nanti. | 2026-08-12 |
| 3 | SSH tunnel | ⏸️ **Tidak untuk sekarang** — koneksi langsung host:port. Bisa ditambahkan belakangan (crate `russh`). | 2026-08-12 |

### Masih terbuka (non-blocking)

1. **Webhook notifikasi?** — Notifikasi backup gagal via Discord/Slack/Email?
2. **Theme?** — Light/dark mode toggle? (Dark sudah default.)

---

*Dokumen ini akan di-update seiring development.*