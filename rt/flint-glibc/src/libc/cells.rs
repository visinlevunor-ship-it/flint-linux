//! Variables a program may have its own copy of (copy relocations): getopt's, the time
//! zone's. After loading, each cell points at the program's copy if there is one.

use crate::loader::Dso;

pub unsafe fn bind(scope: &[*mut Dso]) {
    super::time::bind(scope);
    super::util::bind(scope);
}
