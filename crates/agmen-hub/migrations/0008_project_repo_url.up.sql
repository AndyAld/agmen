ALTER TABLE projects
    ADD COLUMN repo_url text NULL CHECK (repo_url IS NULL OR length(repo_url) <= 512);
