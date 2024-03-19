CREATE TABLE expiring_cache (
    key TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL,
    expires_at DATETIME NOT NULL
);

CREATE INDEX expiring_cache_key_idx ON expiring_cache (key, expires_at DESC);
