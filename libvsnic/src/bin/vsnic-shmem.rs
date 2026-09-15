// Copyright (c) 2026 National Institute of Advanced Industrial Science and Technology (AIST)
// All rights reserved.
//
// This software is released under the MIT License.
// http://opensource.org/licenses/mit-license.php

use libvsnic::linux;
fn main() -> std::io::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or_else(|| std::io::Error::other("usage: vsnic-shmem NEW_FILE"))?;
    linux::create_shmem(std::path::Path::new(&path))
}
