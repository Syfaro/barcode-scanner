CREATE TABLE vci_issuer (
    id INTEGER NOT NULL PRIMARY KEY,
    iss TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    website TEXT,
    canonical_iss TEXT,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    error BOOLEAN NOT NULL
);

CREATE TABLE vci_issuer_key (
    id INTEGER NOT NULL PRIMARY KEY,
    vci_issuer_id INTEGER NOT NULL REFERENCES vci_issuer (id),
    key_id TEXT NOT NULL,
    data TEXT NOT NULL,
    UNIQUE (vci_issuer_id, key_id)
);
