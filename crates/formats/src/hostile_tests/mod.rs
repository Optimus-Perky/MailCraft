//! Hostile HTML: the fixes ported from upstream WordCraft's import hardening, each with the
//! input that broke the v0.3.0 importer MailCraft forked from. MailCraft runs every message it
//! opens, replies to or forwards through `html::import` on the UI thread, so anything here is a
//! mail anyone can send.
//!
//! Timings are wall-clock bounds far above what the fixed code needs (it takes milliseconds) and
//! far below what the quadratic code took (tens of seconds), so a slow CI machine stays green.

mod comments;
mod entities;
mod font_names;
mod font_size;
mod important;
mod link_targets;
mod quadratic_time;
mod raw_text;
mod rowspan;
mod title;
