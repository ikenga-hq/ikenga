---
"ikenga-desktop": patch
---

Database migration 0072 adds the `demos_booked` table (sales demo bookings). It uses `CREATE TABLE IF NOT EXISTS`, so an install that already has a hand-made copy keeps it, rows and all; every other install, including the Ikenga box accounts, gets the same schema.
