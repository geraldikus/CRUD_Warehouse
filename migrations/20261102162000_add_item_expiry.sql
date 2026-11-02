ALTER TABLE items
    ADD COLUMN expires_at TIMESTAMPTZ,
    ADD COLUMN expiry_notice TEXT NOT NULL DEFAULT 'none';
