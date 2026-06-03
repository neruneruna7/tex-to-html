#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! anyhow = "1"
//! flate2 = "1"
//! tar = "0.4"
//! walkdir = "2"
//! ```

use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
use std::{
    env,
    ffi::OsStr,
    fs,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tar::Archive;
use walkdir::WalkDir;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 || args.len() > 3 {
        print_usage(&args[0]);
        bail!("invalid arguments");
    }

    let archive_path = PathBuf::from(&args[1]);
    let tex_main_arg = args.get(2).map(PathBuf::from);

    if !archive_path.is_file() {
        bail!("archive not found: {}", archive_path.display());
    }

    ensure_tar_gz(&archive_path)?;

    let base_name = archive_base_name(&archive_path)?;

    let work_root = PathBuf::from("work");
    let dist_root = PathBuf::from("dist");

    let work_dir = work_root.join(&base_name);
    let output_dir = dist_root.join(&base_name);

    recreate_dir(&work_dir)?;
    recreate_dir(&output_dir)?;

    extract_tar_gz(&archive_path, &work_dir)
        .with_context(|| format!("failed to extract {}", archive_path.display()))?;

    println!(
        "Extracted: {} -> {}",
        archive_path.display(),
        work_dir.display()
    );

    let tex_file_rel = match tex_main_arg {
        Some(path) => {
            let candidate = work_dir.join(&path);

            if !candidate.is_file() {
                bail!("TeX file not found: {}", candidate.display());
            }

            path
        }
        None => find_main_tex_file(&work_dir)?,
    };

    println!("TeX source: {}", work_dir.join(&tex_file_rel).display());

    let config_path = work_dir.join("make4ht-single.cfg");
    write_make4ht_config(&config_path)?;

    let output_dir_abs = fs::canonicalize(&output_dir)
        .with_context(|| format!("failed to canonicalize {}", output_dir.display()))?;

    run_make4ht(&work_dir, &tex_file_rel, &output_dir_abs)?;

    normalize_single_html(&output_dir)?;
    normalize_image_layout(&output_dir)?;

    cleanup_intermediate_files(&work_dir)?;

    println!("Generated: {}", output_dir.join("index.html").display());

    Ok(())
}

fn print_usage(program: &str) {
    eprintln!(
        r#"Usage:
  {program} <sources/archive.tar.gz> [main.tex]

Examples:
  {program} sources/arXiv-1812.00535v3.tar.gz
  {program} sources/arXiv-1812.00535v3.tar.gz main.tex
  {program} sources/arXiv-1812.00535v3.tar.gz src/main.tex

Output:
  work/<archive-name>/
  dist/<archive-name>/index.html"#
    );
}

fn print_latex_error_context(work_dir: &Path) -> Result<()> {
    let mut logs = Vec::new();

    for entry in
        fs::read_dir(work_dir).with_context(|| format!("failed to read {}", work_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.extension() == Some(OsStr::new("log")) {
            logs.push(path);
        }
    }

    for log_path in logs {
        let content = fs::read_to_string(&log_path)
            .with_context(|| format!("failed to read {}", log_path.display()))?;

        let lines: Vec<&str> = content.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            if line.contains("Undefined control sequence") {
                eprintln!();
                eprintln!("==== LaTeX error context from {} ====", log_path.display());

                let start = i.saturating_sub(3);
                let end = usize::min(i + 8, lines.len());

                for j in start..end {
                    eprintln!("{:>6}: {}", j + 1, lines[j]);
                }
            }
        }
    }

    Ok(())
}

fn ensure_tar_gz(path: &Path) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("invalid archive file name: {}", path.display()))?;

    if !file_name.ends_with(".tar.gz") {
        bail!("archive must end with .tar.gz: {}", path.display());
    }

    Ok(())
}

fn archive_base_name(path: &Path) -> Result<String> {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("invalid archive file name: {}", path.display()))?;

    let base = file_name
        .strip_suffix(".tar.gz")
        .ok_or_else(|| anyhow!("archive must end with .tar.gz: {}", path.display()))?;

    if base.is_empty() {
        bail!("empty archive base name: {}", path.display());
    }

    Ok(base.to_string())
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }

    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;

    Ok(())
}

fn extract_tar_gz(archive_path: &Path, extract_dir: &Path) -> Result<()> {
    let file = File::open(archive_path)
        .with_context(|| format!("failed to open {}", archive_path.display()))?;

    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    archive
        .unpack(extract_dir)
        .with_context(|| format!("failed to unpack into {}", extract_dir.display()))?;

    Ok(())
}
fn find_main_tex_file(work_dir: &Path) -> Result<PathBuf> {
    let conventional_names = [
        "main.tex",
        "0-main.tex",
        "example_paper.tex",
        "paper.tex",
        "article.tex",
        "manuscript.tex",
        "ms.tex",
    ];

    for name in conventional_names {
        let candidate = work_dir.join(name);

        if candidate.is_file() {
            return Ok(PathBuf::from(name));
        }
    }

    let mut tex_files = Vec::new();

    for entry in WalkDir::new(work_dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();

        if path.is_file() && path.extension() == Some(OsStr::new("tex")) {
            let rel = path
                .strip_prefix(work_dir)
                .with_context(|| {
                    format!(
                        "failed to strip prefix: {} from {}",
                        work_dir.display(),
                        path.display()
                    )
                })?
                .to_path_buf();

            tex_files.push(rel);
        }
    }

    match tex_files.len() {
        0 => bail!("no .tex file found under {}", work_dir.display()),
        1 => Ok(tex_files.remove(0)),
        _ => {
            eprintln!("Multiple .tex files found and no conventional main file was selected:");
            eprintln!("Looked for these top-level names:");

            for name in conventional_names {
                eprintln!("  {name}");
            }

            eprintln!();
            eprintln!("Found .tex files:");

            for path in &tex_files {
                eprintln!("  {}", path.display());
            }

            bail!("specify the main TeX file as the second argument");
        }
    }
}

fn write_make4ht_config(path: &Path) -> Result<()> {
    let config = r#"\Preamble{xhtml}

\Configure{CutAt}{}

% TeX4ht sometimes references this internal section-title macro
% after ignoring appendix sections.
\expandafter\def\csname ssect:ttl\endcsname{}

% Common paper macros that may be undefined under htlatex.
\providecommand{\etal}{et al.}
\providecommand{\eg}{e.g.}
\providecommand{\ie}{i.e.}
\providecommand{\cf}{cf.}

% Cross-reference fallbacks.
\providecommand{\cref}[1]{\ref{#1}}
\providecommand{\Cref}[1]{\ref{#1}}
\providecommand{\autoref}[1]{\ref{#1}}

% Annotation commands often used in drafts.
\providecommand{\todo}[1]{}
\providecommand{\TODO}[1]{}
\providecommand{\note}[1]{}
\providecommand{\comment}[1]{}

% IEEE-related fallbacks.
\providecommand{\IEEEpubid}[1]{}
\providecommand{\IEEEpubidadjcol}{}
\providecommand{\IEEEpeerreviewmaketitle}{}
\providecommand{\IEEEauthorblockN}[1]{#1}
\providecommand{\IEEEauthorblockA}[1]{#1}
\providecommand{\IEEEoverridecommandlockouts}{}

\Css{
:root {
  color-scheme: light;
}
html, body {
  background: white;
  color: black;
}
img:not(.math) {
  max-width: 95\%;
  height: auto;
}
}

\begin{document}
\EndPreamble
"#;

    fs::write(path, config).with_context(|| format!("failed to write {}", path.display()))?;

    Ok(())
}

fn run_make4ht(work_dir: &Path, tex_file_rel: &Path, output_dir_abs: &Path) -> Result<()> {
    let status = Command::new("make4ht")
        .current_dir(work_dir)
        .arg("-d")
        .arg(output_dir_abs)
        .arg("-c")
        .arg("make4ht-single.cfg")
        .arg(tex_file_rel)
        .arg("html5,mathjax,fn-in")
        .stdin(Stdio::null())
        .status()
        .context("failed to execute make4ht; is MacTeX/TeX Live installed and PATH configured?")?;

    if !status.success() {
        print_latex_error_context(work_dir)?;
        bail!("make4ht failed with status: {status}");
    }

    Ok(())
}
fn normalize_single_html(output_dir: &Path) -> Result<()> {
    let mut html_files = Vec::new();

    for entry in fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension() == Some(OsStr::new("html")) {
            html_files.push(path);
        }
    }

    match html_files.len() {
        0 => bail!("no HTML file generated under {}", output_dir.display()),
        1 => {
            let html_file = html_files.remove(0);
            let index = output_dir.join("index.html");

            if html_file != index {
                if index.exists() {
                    fs::remove_file(&index)
                        .with_context(|| format!("failed to remove {}", index.display()))?;
                }

                fs::rename(&html_file, &index).with_context(|| {
                    format!(
                        "failed to rename {} to {}",
                        html_file.display(),
                        index.display()
                    )
                })?;
            }

            Ok(())
        }
        _ => {
            eprintln!(
                "Expected exactly one HTML file, but found {}:",
                html_files.len()
            );

            for path in &html_files {
                eprintln!("  {}", path.display());
            }

            bail!(
                "make4ht generated multiple HTML files; the current config did not fully prevent splitting"
            );
        }
    }
}

fn normalize_image_layout(output_dir: &Path) -> Result<()> {
    rewrite_img_dimensions(output_dir)?;

    for entry in fs::read_dir(output_dir)
        .with_context(|| format!("failed to read {}", output_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension() == Some(OsStr::new("css")) {
            append_responsive_image_css(&path)?;
        }
    }

    Ok(())
}

fn rewrite_img_dimensions(output_dir: &Path) -> Result<()> {
    let index_path = output_dir.join("index.html");
    let html = fs::read_to_string(&index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;

    let mut output = String::with_capacity(html.len());
    let mut rest = html.as_str();

    while let Some(start) = rest.find("<img") {
        output.push_str(&rest[..start]);
        rest = &rest[start..];

        let Some(end) = rest.find('>') else {
            output.push_str(rest);
            rest = "";
            break;
        };

        let tag = &rest[..=end];
        output.push_str(&rewrite_img_tag_dimensions(tag, output_dir)?);
        rest = &rest[end + 1..];
    }

    output.push_str(rest);
    fs::write(&index_path, output)
        .with_context(|| format!("failed to write {}", index_path.display()))?;

    Ok(())
}

fn rewrite_img_tag_dimensions(tag: &str, output_dir: &Path) -> Result<String> {
    let Some(src) = attr_value(tag, "src") else {
        return Ok(tag.to_string());
    };

    let Some(image_path) = image_src_path(output_dir, &src) else {
        return Ok(tag.to_string());
    };

    let Some((natural_width, natural_height)) = image_dimensions(&image_path)? else {
        return Ok(tag.to_string());
    };

    let display_width = attr_u32(tag, "width").unwrap_or(natural_width);
    let display_height = ((display_width as f64) * (natural_height as f64) / (natural_width as f64))
        .round()
        .max(1.0) as u32;

    let tag = set_attr_value(tag, "width", &display_width.to_string());
    let tag = set_attr_value(&tag, "height", &display_height.to_string());

    Ok(tag)
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let start = tag.find(&needle)? + needle.len();
    let quote = tag[start..].chars().next()?;

    if quote != '\'' && quote != '"' {
        return None;
    }

    let value_start = start + quote.len_utf8();
    let value_end = tag[value_start..].find(quote)? + value_start;

    Some(tag[value_start..value_end].to_string())
}

fn attr_u32(tag: &str, name: &str) -> Option<u32> {
    attr_value(tag, name)?.parse().ok()
}

fn set_attr_value(tag: &str, name: &str, value: &str) -> String {
    let needle = format!("{name}=");

    if let Some(start) = tag.find(&needle) {
        let quote_index = start + needle.len();
        let Some(quote) = tag[quote_index..].chars().next() else {
            return tag.to_string();
        };

        if quote != '\'' && quote != '"' {
            return tag.to_string();
        }

        let value_start = quote_index + quote.len_utf8();
        let Some(value_end_rel) = tag[value_start..].find(quote) else {
            return tag.to_string();
        };

        let value_end = value_start + value_end_rel;
        let mut rewritten = String::with_capacity(tag.len() + value.len());
        rewritten.push_str(&tag[..value_start]);
        rewritten.push_str(value);
        rewritten.push_str(&tag[value_end..]);
        return rewritten;
    }

    let Some(end) = tag.rfind('>') else {
        return tag.to_string();
    };

    let mut rewritten = String::with_capacity(tag.len() + name.len() + value.len() + 4);
    rewritten.push_str(&tag[..end]);
    rewritten.push(' ');
    rewritten.push_str(name);
    rewritten.push_str("='");
    rewritten.push_str(value);
    rewritten.push('\'');
    rewritten.push_str(&tag[end..]);
    rewritten
}

fn image_src_path(output_dir: &Path, src: &str) -> Option<PathBuf> {
    let src = src.split(['?', '#']).next().unwrap_or(src);

    if src.starts_with("http://") || src.starts_with("https://") || src.starts_with("data:") {
        return None;
    }

    let src = src.strip_prefix("./").unwrap_or(src);
    let path = output_dir.join(src);

    path.is_file().then_some(path)
}

fn image_dimensions(path: &Path) -> Result<Option<(u32, u32)>> {
    match path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
    {
        Some(ext) if ext == "png" => png_dimensions(path),
        Some(ext) if ext == "jpg" || ext == "jpeg" => jpeg_dimensions(path),
        Some(ext) if ext == "svg" => svg_dimensions(path),
        _ => Ok(None),
    }
}

fn png_dimensions(path: &Path) -> Result<Option<(u32, u32)>> {
    let mut header = [0u8; 24];
    let bytes_read = File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .read(&mut header)
        .with_context(|| format!("failed to read PNG header from {}", path.display()))?;

    if bytes_read < header.len() {
        return Ok(None);
    }

    if &header[..8] != b"\x89PNG\r\n\x1a\n" {
        return Ok(None);
    }

    let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);

    Ok(Some((width, height)))
}

fn jpeg_dimensions(path: &Path) -> Result<Option<(u32, u32)>> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;

    if bytes.len() < 4 || bytes[..2] != [0xff, 0xd8] {
        return Ok(None);
    }

    let mut i = 2usize;

    while i + 9 < bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }

        while i < bytes.len() && bytes[i] == 0xff {
            i += 1;
        }

        if i >= bytes.len() {
            break;
        }

        let marker = bytes[i];
        i += 1;

        if marker == 0xd9 || marker == 0xda {
            break;
        }

        if i + 2 > bytes.len() {
            break;
        }

        let segment_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;

        if segment_len < 2 || i + segment_len > bytes.len() {
            break;
        }

        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            let height = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let width = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            return Ok(Some((width, height)));
        }

        i += segment_len;
    }

    Ok(None)
}

fn svg_dimensions(path: &Path) -> Result<Option<(u32, u32)>> {
    let svg =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    if let (Some(width), Some(height)) = (
        svg_number_attr(&svg, "width"),
        svg_number_attr(&svg, "height"),
    ) {
        return Ok(Some((width.round() as u32, height.round() as u32)));
    }

    if let Some(view_box) = attr_value(&svg, "viewBox").or_else(|| attr_value(&svg, "viewbox")) {
        let nums: Vec<f64> = view_box
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter_map(|part| part.parse::<f64>().ok())
            .collect();

        if nums.len() == 4 {
            return Ok(Some((nums[2].round() as u32, nums[3].round() as u32)));
        }
    }

    Ok(None)
}

fn svg_number_attr(svg: &str, name: &str) -> Option<f64> {
    let value = attr_value(svg, name)?;
    let number: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();

    number.parse().ok()
}

fn append_responsive_image_css(path: &Path) -> Result<()> {
    let mut css =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let rule = "\nimg:not(.math) { max-width: 95%; height: auto; }\n";

    if !css.contains("img:not(.math) { max-width: 95%; height: auto; }") {
        css.push_str(rule);
        fs::write(path, css).with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(())
}

fn cleanup_intermediate_files(work_dir: &Path) -> Result<()> {
    let removable_extensions = [
        "aux", "log", "xref", "4ct", "4tc", "lg", "tmp", "dvi", "idv", "out",
    ];

    for entry in
        fs::read_dir(work_dir).with_context(|| format!("failed to read {}", work_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let Some(ext) = path.extension().and_then(OsStr::to_str) else {
            continue;
        };

        if removable_extensions.contains(&ext) {
            let _ = fs::remove_file(&path);
        }
    }

    Ok(())
}
