CREATE TABLE cvx_code (
    code INTEGER NOT NULL PRIMARY KEY,
    short_description TEXT NOT NULL,
    full_name TEXT NOT NULL,
    vaccine_status TEXT NOT NULL,
    last_updated DATE NOT NULL,
    notes TEXT
);
