//! The activity feed's shared evaluation: the search grammar, the row and
//! the table. The mux client and the fno-agent held reader both run this one
//! copy, so a query and a view mean the same thing on either side. Behind
//! the `feed` feature, so a wire-only client never links it.

pub mod grammar;
pub mod row;
pub mod serve;
pub mod table;
