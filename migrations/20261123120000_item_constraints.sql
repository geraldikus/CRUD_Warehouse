-- The API already checks these. The database checks them too, so a row
-- written by any other client (psql, a script) cannot break the rules.
-- 200 matches MAX_NAME_CHARS in warehouse-core.
ALTER TABLE items
    ADD CONSTRAINT items_quantity_non_negative CHECK (quantity >= 0),
    ADD CONSTRAINT items_name_not_blank CHECK (btrim(name) <> ''),
    ADD CONSTRAINT items_name_length CHECK (char_length(name) <= 200),
    ADD CONSTRAINT items_expiry_notice_known
        CHECK (expiry_notice IN ('none', 'day', 'hour', 'expired'));
