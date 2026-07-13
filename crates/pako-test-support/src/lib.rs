use std::path::Path;

use pako_core::layout::Layout;

/// Temporary XDG layout used by lifecycle tests.
#[derive(Debug)]
pub struct TestLayout {
    temporary: tempfile::TempDir,
    pub layout: Layout,
}

impl TestLayout {
    pub fn new() -> Self {
        let temporary = tempfile::tempdir().expect("create temporary directory");
        let layout = Layout::for_test(temporary.path());
        layout.ensure().expect("create test layout");

        Self { temporary, layout }
    }

    pub fn root(&self) -> &Path {
        self.temporary.path()
    }
}

impl Default for TestLayout {
    fn default() -> Self {
        Self::new()
    }
}

pub fn write_sample_tree(root: &Path) {
    std::fs::create_dir_all(root.join("bin")).expect("create sample bin directory");
    std::fs::create_dir_all(root.join("lib")).expect("create sample lib directory");
    std::fs::write(root.join("bin/example"), b"#!/bin/sh\necho pako\n")
        .expect("write sample executable");
    std::fs::write(root.join("lib/data.bin"), vec![0x5a; 2 * 1024 * 1024])
        .expect("write sample data file");
}
