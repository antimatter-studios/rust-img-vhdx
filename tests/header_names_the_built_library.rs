//! The header names the library the build actually produces.
//!
//! `include/vhdx.h` said "Link with libam_img_vhdx.a" for as long as the file
//! existed, and cargo has never produced that name: `[lib] name` is
//! `vhdx`, so the artefact is `libvhdx.a` — which is exactly what
//! `chores.yml` copies. A C consumer following the header got a linker
//! error for a library nobody builds.
//!
//! **The expected name is DERIVED from `Cargo.toml`, not written down
//! here**, so renaming the library fails this test rather than silently
//! making the header wrong again.
//!
//! # Why `toml` and not a hand parse
//!
//! The first version of this file hand-parsed `Cargo.toml` by scanning
//! lines, and rejected three spellings cargo accepts: `name = 'vhdx'`
//! in single quotes, `name = "vhdx" # comment` with a trailing comment,
//! and `[lib] # comment` — the last making the test claim the manifest
//! declares no library at all. A guard that a legal edit to the file it
//! guards can break is one somebody deletes rather than fixes.
//!
//! A guard asserting something about a structured file must PARSE it.
//! `toml` is a dev-dependency only, so nothing reaches a consumer.
//!
//! The C header is the exception, and deliberately: there is no parser
//! for it here, so `libraries_named` scans. That is a stated limit
//! rather than a quiet one.

use std::path::Path;

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// `[lib] name`, parsed.
fn lib_name(cargo_toml: &str) -> Option<String> {
    let doc: toml::Value = toml::from_str(cargo_toml).ok()?;
    Some(doc.get("lib")?.get("name")?.as_str()?.to_owned())
}

/// Every `lib<something>.a` the header mentions, anywhere.
///
/// A scan, because a C header has no parser here — see the module
/// note. It is deliberately context-free, which is why it is not the
/// only thing asserted: see [`states_the_link_instruction`].
fn libraries_named(header: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in header.lines() {
        let mut rest = line;
        while let Some(i) = rest.find("lib") {
            let tail = &rest[i..];
            let name: String = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            if name.ends_with(".a") && !out.contains(&name) {
                out.push(name);
            }
            rest = &rest[i + 3..];
        }
    }
    out
}

/// Whether the header AFFIRMATIVELY tells a consumer to link `want`.
///
/// `libraries_named` alone cannot say this. It matches any mention, so
/// a header whose only sentence is "do not link with libfoo.a" — or
/// which names the library in passing while giving no instruction at
/// all — satisfies a check built on it. The point of the file is that
/// a C consumer reading the header knows what to link, and "mentioned
/// somewhere" is not that.
///
/// THE INSTRUCTION MUST OPEN THE SENTENCE. After comment decoration
/// (`*`, `/`, `#`) and leading space, the line has to begin `link with
/// <want>`.
///
/// The first version of this searched for the phrase ANYWHERE on the
/// line, and so accepted `Do not link with <want>` — a header telling
/// a consumer explicitly not to link the library passed a test whose
/// purpose is to confirm it says to link it. The failure message below
/// already claimed that case was caught; the predicate did not
/// implement it, which is this file's own defect shape one level in.
///
/// It stays a rule about how the sentence STARTS rather than a
/// negation blacklist, because a blacklist is a list of the negations
/// someone thought of. Refusing `To use this, link with <want>` is the
/// price, and it fails loudly with the line quoted.
///
/// # THE PHRASE IS CASE-INSENSITIVE; THE FILENAME IS NOT
///
/// This lowercased the whole LINE and compared it against a `want`
/// that was not lowercased. `want` is `format!("lib{name}.a")` with
/// `name` read verbatim from `[lib] name`, so a manifest declaring
/// `Vhdx` produced `libVhdx.a`, the header's correct `Link with
/// libVhdx.a` was lowered to `libvhdx.a`, and the guard REJECTED a
/// header that was right. Because it runs first in the `staticlib`
/// task, that stops packaging before the release build.
///
/// **Lowercasing `want` too would have been the wrong repair.** It
/// makes the comparison case-insensitive on both sides, so a header
/// saying `libvhdx.a` while cargo builds `libVhdx.a` would pass — and
/// a linker is case-sensitive about a filename, so the consumer is
/// then told to link a file that does not exist. It would trade a loud
/// false rejection for a silent false acceptance, which is the worse
/// direction and the one this file exists to close.
///
/// So only the PHRASE is matched without regard to case, and the
/// library name is compared exactly. `want` is untouched: it also
/// builds the `include/{name}.h` path and appears in the failure
/// messages, and lowercasing it at the source would look for the wrong
/// header on precisely the crates this concerns.
fn states_the_link_instruction(header: &str, want: &str) -> bool {
    /// Written lower-case; matched against the line without regard to
    /// case.
    const PHRASE: &str = "link with ";

    header.lines().any(|line| {
        let bare = line.trim_start().trim_start_matches(['*', '/', '#', ' ']);
        // `get`, not `split_at`: a line whose tenth byte falls inside a
        // multi-byte character would panic, and a header is free to
        // contain one.
        let Some(head) = bare.get(..PHRASE.len()) else {
            return false;
        };
        if !head.eq_ignore_ascii_case(PHRASE) {
            return false;
        }
        bare[PHRASE.len()..].trim_start().starts_with(want)
    })
}

#[test]
fn the_header_tells_consumers_to_link_the_library_that_is_built() {
    let cargo = read("Cargo.toml");
    let name = lib_name(&cargo).expect("Cargo.toml declares [lib] name");
    let want = format!("lib{name}.a");

    // Named explicitly: a missing header here almost always means the
    // manifest and the shipped files have drifted apart, and "No such
    // file" on its own does not say so.
    let header_path = format!("include/{name}.h");
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(&header_path);
    assert!(
        full.exists(),
        "Cargo.toml declares [lib] name {name:?}, so the build produces {want} and \
         the C header for it should be {header_path} -- which does not exist. The \
         manifest and the shipped headers have drifted apart."
    );
    let header = read(&header_path);
    let named = libraries_named(&header);

    // Asserted before it is compared: a scan that found nothing would
    // make the loop below pass over an empty list, which is the shape
    // of defect this file exists for.
    assert!(
        !named.is_empty(),
        "include/{name}.h names no lib*.a at all, so it gives a C consumer no link \
         guidance. It should name {want}."
    );

    assert!(
        states_the_link_instruction(&header, &want),
        "include/{name}.h mentions {named:?} but never says \"Link with {want}\". A \
         consumer reading it is not told what to link, and a mention in passing — or \
         in a sentence saying NOT to link something — is not an instruction."
    );

    for got in &named {
        assert_eq!(
            got, &want,
            "include/{name}.h tells consumers to link {got}, but Cargo.toml's \
             [lib] name is {name:?}, so the build produces {want}. Linking {got} \
             fails: nothing builds it."
        );
    }
}

/// THE SPELLINGS A HAND PARSE GOT WRONG.
///
/// Every one of these is valid TOML that cargo accepts, and every one
/// of them broke the previous version of this file — two by reading a
/// mangled name, one by concluding there was no `[lib]` section. They
/// are here as acceptance cases rather than rejection cases, because
/// the fix has to be shown to ACCEPT what it used to refuse; a parser
/// that merely still handles the plain spelling proves nothing.
#[test]
fn the_lib_name_is_parsed_rather_than_scanned() {
    let plain = "[package]\nname = \"am-img-vhdx\"\n\n[lib]\nname = \"vhdx\"\n";
    let single_quoted = "[package]\nname = \"am-img-vhdx\"\n\n[lib]\nname = 'vhdx'\n";
    let trailing_comment =
        "[package]\nname = \"am-img-vhdx\"\n\n[lib]\nname = \"vhdx\" # the exported ABI name\n";
    let commented_section =
        "[package]\nname = \"am-img-vhdx\"\n\n[lib] # the staticlib consumers link\nname = \"vhdx\"\n";

    for (what, toml) in [
        ("the plain spelling", plain),
        ("a single-quoted string", single_quoted),
        ("a trailing comment", trailing_comment),
        ("a comment on the section header", commented_section),
    ] {
        assert_eq!(
            lib_name(toml).as_deref(),
            Some("vhdx"),
            "{what} is valid TOML and cargo accepts it, so this guard must too"
        );
    }
}

/// `vars.LIBNAME` out of `chores.yml`, parsed.
///
/// YAML, so it is parsed rather than scanned — `saphyr` is the adopted
/// parser for it, the way `toml` is for the manifest.
fn chores_libname(chores_yml: &str) -> Option<String> {
    use saphyr::{LoadableYamlNode, Yaml};
    let docs = Yaml::load_from_str(chores_yml).ok()?;
    let doc = docs.first()?;
    // as_mapping_get, not indexing: saphyr's Index PANICS on a missing
    // key, so a chores file without the variable would abort the test
    // rather than report its absence -- and reporting absence is half
    // of what this function is for.
    doc.as_mapping_get("vars")?
        .as_mapping_get("LIBNAME")?
        .as_str()
        .map(|s| s.to_owned())
}

/// THE PACKAGING VARIABLE AGREES WITH THE MANIFEST.
///
/// `chores.yml` copies `target/<triple>/release/lib{{.LIBNAME}}.a`, but
/// what cargo builds is named by `[lib] name`. Nothing tied the two
/// together: the header check compares against the manifest, and
/// `LIBNAME` was free to drift from it independently.
///
/// A drift did fail — but LATE and unrecognisably, after a full release
/// cross-compile, as `cp: cannot stat .../libNAME.a`. The guard runs
/// first precisely so a naming mistake costs no build, and this was the
/// one naming mistake it did not cover.
#[test]
fn the_packaging_variable_matches_the_manifest() {
    let name = lib_name(&read("Cargo.toml")).expect("Cargo.toml declares [lib] name");
    let libname = chores_libname(&read("chores.yml")).expect("chores.yml declares vars.LIBNAME");
    assert_eq!(
        libname, name,
        "chores.yml sets LIBNAME={libname:?} and Cargo.toml sets [lib] name={name:?}. \
         cargo builds lib{name}.a, chores copies lib{libname}.a, and the packaging step \
         fails with `cp: cannot stat` after the release build rather than here."
    );
}

/// The chores parse reads YAML rather than matching a line.
#[test]
fn the_libname_is_parsed_rather_than_scanned() {
    for (what, yml) in [
        ("a plain value", "vars:\n  LIBNAME: vhdx\n"),
        ("a quoted value", "vars:\n  LIBNAME: \"vhdx\"\n"),
        (
            "a trailing comment",
            "vars:\n  LIBNAME: vhdx # the linked name\n",
        ),
        (
            "the key named in a comment first",
            "# LIBNAME: wrong\nvars:\n  LIBNAME: vhdx\n",
        ),
    ] {
        assert_eq!(
            chores_libname(yml).as_deref(),
            Some("vhdx"),
            "{what} is valid YAML, so this guard must read it"
        );
    }
    assert_eq!(
        chores_libname("tasks:\n  build:\n    cmds: ['cargo build']\n"),
        None,
        "a chores file with no LIBNAME has none to report"
    );
}

/// The package name is not the library name.
#[test]
fn the_lib_name_comes_from_the_lib_section_and_not_the_package() {
    let toml = "[package]\nname = \"am-img-vhdx\"\nversion = \"0.3.5\"\n\n\
                [lib]\nname = \"vhdx\"\ncrate-type = [\"staticlib\", \"rlib\"]\n";
    assert_eq!(
        lib_name(toml).as_deref(),
        Some("vhdx"),
        "using the package name would look for libam-img-vhdx.a"
    );
    // And a manifest with no [lib] section has no library name to give.
    assert_eq!(lib_name("[package]\nname = \"am-img-vhdx\"\n"), None);
}

/// AN INSTRUCTION, NOT A MENTION — AND NOT A NEGATION.
///
/// The rejection half is the filed defect: `Do not link with
/// libvhdx.a` satisfied a check for "does the header say to link it".
///
/// The acceptance half is the one that gets forgotten. A stricter
/// matcher that closed the negation gap by refusing the real header —
/// or the same sentence behind `//`, `#`, or a closing `*/` — would be
/// a worse guard than the gap it removed, so both directions are
/// asserted here rather than only the one the issue named.
#[test]
fn the_link_instruction_must_open_the_sentence() {
    let want = "libvhdx.a";

    for accepted in [
        " * Link with libvhdx.a alongside fs_core.h.\n",
        "Link with libvhdx.a\n",
        "// Link with libvhdx.a and include this header.\n",
        "  # link with   libvhdx.a\n",
        " */ Link with libvhdx.a\n",
        " * unrelated first line\n * Link with libvhdx.a\n",
    ] {
        assert!(
            states_the_link_instruction(accepted, want),
            "{accepted:?} tells a consumer to link {want} and must be read as one"
        );
    }

    for refused in [
        // The filed defect.
        " * Do not link with libvhdx.a; it is an implementation detail.\n",
        " * You must never link with libvhdx.a directly.\n",
        // A mention with no instruction: what #76's first fix caught.
        " * The build produces libvhdx.a in the target directory.\n",
        " * libvhdx.a was renamed in 0.4.0.\n",
        // An instruction naming a different library.
        " * Link with libfs_core.a.\n",
        // Nothing at all.
        "",
    ] {
        assert!(
            !states_the_link_instruction(refused, want),
            "{refused:?} does not tell a consumer to link {want}"
        );
    }
}

/// A MIXED-CASE `[lib] name` IS A CORRECT MANIFEST, AND ITS HEADER
/// MUST PASS.
///
/// The whole line was lowercased and `want` was not, so a crate
/// declaring `[lib] name = "Vhdx"` had its correct `Link with
/// libVhdx.a` rejected — in the packaging step, after a green CI.
/// Unreachable in this repository, where the name is already
/// lowercase, and unwitnessed too: no test above uses a name with an
/// uppercase character in it, which is why the defect could sit here
/// waiting to be copied into a crate where it bites.
///
/// The three assertions are three different claims and each fails on
/// its own:
///
/// - the filed defect, which the old predicate got wrong;
/// - the phrase is still read whatever its case, which is what the
///   line-lowercasing was doing and had to be preserved;
/// - the filename is NOT, which is the false acceptance that
///   lowercasing `want` would have introduced.
#[test]
fn a_mixed_case_library_name_is_read_as_itself() {
    assert!(
        states_the_link_instruction(" * Link with libVhdx.a alongside fs_core.h.\n", "libVhdx.a"),
        "a manifest may declare a mixed-case [lib] name, and a header naming that \
         library exactly is telling a consumer the truth"
    );

    for spelling in [
        " * LINK WITH libvhdx.a\n",
        " * Link With libvhdx.a\n",
        " * link with libvhdx.a\n",
    ] {
        assert!(
            states_the_link_instruction(spelling, "libvhdx.a"),
            "{spelling:?} is the instruction; only its case differs"
        );
    }

    assert!(
        !states_the_link_instruction(" * Link with libvhdx.a\n", "libVhdx.a"),
        "cargo builds libVhdx.a and the header says libvhdx.a. A linker is \
         case-sensitive about a filename, so this header sends a consumer after a \
         file that is not there -- accepting it is the silent failure that \
         lowercasing both sides would have introduced"
    );
}

/// The header scan finds a library name wherever it sits in a line.
#[test]
fn the_header_scan_finds_library_names_in_prose() {
    let header = " * Link with libvhdx.a alongside fs_core.h.\n";
    assert_eq!(libraries_named(header), vec!["libvhdx.a"]);

    // The exact defect, and the near-miss that has to stay distinct.
    assert_eq!(
        libraries_named(" * Link with libam_img_vhdx.a and include this\n"),
        vec!["libam_img_vhdx.a"]
    );

    // Words merely beginning with "lib" are not libraries.
    assert!(libraries_named(" * This library is liberally licensed.\n").is_empty());
}
