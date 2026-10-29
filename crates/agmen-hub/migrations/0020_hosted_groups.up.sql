-- Hosted group plans and the one trial grant per GitHub account.
-- trial_grants has no foreign key, so a grant outlives a deleted group.
-- Migrations 0011 to 0019 stay free on purpose.

CREATE TABLE tenant_plans (
    tenant_id text PRIMARY KEY REFERENCES tenants (id),
    state text NOT NULL CHECK (state IN ('trial', 'active', 'past_due', 'read_only', 'deleted')),
    plan text CHECK (plan IN ('team', 'business')),
    extra_seats int NOT NULL CHECK (extra_seats >= 0),
    business_step int CHECK (business_step IN (10, 25, 50)),
    trial_started_at timestamptz,
    past_due_since timestamptz,
    read_only_cause text CHECK (
        read_only_cause IN ('trial_ended', 'awaiting_payment', 'payment_failed', 'canceled')
    ),
    read_only_since timestamptz,
    reminders_sent smallint NOT NULL CHECK (reminders_sent >= 0),
    last_event_at timestamptz,
    paddle_customer_id text,
    paddle_subscription_id text UNIQUE,
    no_seat_alerted_at timestamptz
);

CREATE TABLE trial_grants (
    github_id text PRIMARY KEY,
    granted_at timestamptz NOT NULL
);
