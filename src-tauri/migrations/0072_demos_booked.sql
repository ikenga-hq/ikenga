-- 0072_demos_booked.sql
-- Sales demo bookings (team ops). The table predates this file: it was created
-- by hand on the founder's desktop, so IF NOT EXISTS keeps that copy as it is
-- and gives every other install (the Ikenga box accounts) the same schema.
-- deal_id refers to sales_deals.id without a declared foreign key, as on the
-- desktop.

CREATE TABLE IF NOT EXISTS demos_booked (
    id                   TEXT PRIMARY KEY,
    prospect_name        TEXT,
    company              TEXT,
    contact_email        TEXT,
    source_channel       TEXT,
    deal_id              TEXT,
    status               TEXT,
    booked_at            TEXT,
    scheduled_for        TEXT,
    booking_url          TEXT,
    excluded_from_target INTEGER DEFAULT 0,
    notes                TEXT,
    created_at           TEXT,
    updated_at           TEXT
);
