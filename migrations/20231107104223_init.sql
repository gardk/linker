CREATE TABLE links (
  slug text PRIMARY KEY,
  url  text NOT NULL UNIQUE
);
