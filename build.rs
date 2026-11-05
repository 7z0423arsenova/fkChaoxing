//! 构建时把字体映射表压缩后嵌入程序，减少二进制体积。
//!
//! 源文件仍保留为可读的 `resource/font_map_table.json`，方便审查和重新生成；
//! 运行时只加载 `OUT_DIR/font_map_table.json.gz`。

use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let source = manifest_dir.join("resource/font_map_table.json");
    println!("cargo:rerun-if-changed={}", source.display());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let target = out_dir.join("font_map_table.json.gz");
    let data = fs::read(&source).expect("读取字体映射表失败");

    let file = File::create(&target).expect("创建压缩字体映射表失败");
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    encoder.write_all(&data).expect("压缩字体映射表失败");
    encoder.finish().expect("写入压缩字体映射表失败");
}
