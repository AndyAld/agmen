CREATE TABLE tenant_members (
    tenant_id text NOT NULL REFERENCES tenants (id),
    human_id text NOT NULL REFERENCES humans (id),
    role text NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, human_id)
);

CREATE UNIQUE INDEX tenant_members_one_owner ON tenant_members (tenant_id)
WHERE role = 'owner';

CREATE INDEX tenant_members_human ON tenant_members (human_id);

INSERT INTO tenant_members (tenant_id, human_id, role)
SELECT p.tenant_id, m.human_id,
       CASE WHEN bool_or(m.role = 'admin') THEN 'admin' ELSE 'member' END
FROM memberships m JOIN projects p ON p.id = m.project_id
GROUP BY p.tenant_id, m.human_id
ON CONFLICT (tenant_id, human_id) DO NOTHING;

-- A claimed bootstrap takes precedence over the oldest project admin.
WITH candidates AS (
    SELECT p.tenant_id, b.claimed_by AS human_id, b.claimed_at AS chosen_at, 0 AS priority
    FROM bootstrap_admins b JOIN projects p ON p.id = b.project_id
    JOIN tenant_members tm ON tm.tenant_id = p.tenant_id AND tm.human_id = b.claimed_by
    WHERE b.claimed_by IS NOT NULL
    UNION ALL
    SELECT p.tenant_id, m.human_id, m.created_at, 1
    FROM memberships m JOIN projects p ON p.id = m.project_id
    WHERE m.role = 'admin'
), owners AS (
    SELECT DISTINCT ON (tenant_id) tenant_id, human_id
    FROM candidates
    ORDER BY tenant_id, priority, chosen_at, human_id
)
UPDATE tenant_members tm SET role = 'owner'
FROM owners o
WHERE tm.tenant_id = o.tenant_id AND tm.human_id = o.human_id;
