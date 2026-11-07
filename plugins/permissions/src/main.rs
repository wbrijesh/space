//! `space-permissions`: the permissions service plugin (see README.md).

use space_plugin::Plugin;

struct Permissions;

impl Plugin for Permissions {}

fn main() {
    eprintln!("space-permissions: started");
    space_plugin::run(Permissions);
}
