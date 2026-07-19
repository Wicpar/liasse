-- Install the extension in template1 so every later CREATE DATABASE inherits
-- it, and in the default database directly.
\c template1
CREATE EXTENSION IF NOT EXISTS liasse_demo;
\c postgres
CREATE EXTENSION IF NOT EXISTS liasse_demo;
