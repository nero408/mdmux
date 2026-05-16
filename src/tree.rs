//! Markdown file tree.
//!
//! Walks a root directory, collects markdown files, and exposes a flat list of
//! visible rows (depth + label + path) that the TUI can render and navigate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Recognised markdown file extensions (lowercase).
const MD_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mkd", "mkdn"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub kind: NodeKind,
    pub depth: usize,
    /// Sorted child paths. Empty for files; for directories, only contains
    /// descendants that ultimately reach at least one markdown file.
    pub children: Vec<PathBuf>,
}

/// One visible row in the rendered tree.
#[derive(Debug, Clone)]
pub struct Row {
    pub path: PathBuf,
    pub depth: usize,
    pub kind: NodeKind,
    pub expanded: bool,
    pub name: String,
    /// True if the row matches the active filter (always true if no filter).
    pub matches: bool,
}

#[derive(Debug, Clone)]
pub struct TreeConfig {
    pub show_hidden: bool,
    pub respect_gitignore: bool,
    pub max_depth: Option<usize>,
}

impl Default for TreeConfig {
    fn default() -> Self {
        Self {
            show_hidden: false,
            respect_gitignore: true,
            max_depth: None,
        }
    }
}

#[derive(Debug)]
pub struct Tree {
    pub root: PathBuf,
    /// Sorted by path. Root itself is not included as a node.
    nodes: Vec<Node>,
    /// Indexes of nodes for quick lookup by path.
    expanded: HashSet<PathBuf>,
    filter: String,
    /// Cached visible rows; invalidated on structural change.
    cache_dirty: bool,
    cached_rows: Vec<Row>,
}

impl Tree {
    /// Build the tree by walking `root`. Returns an empty tree (no markdown
    /// files) if the walker found none.
    pub fn build(root: impl AsRef<Path>, cfg: &TreeConfig) -> anyhow::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        let nodes = walk_markdown(&root, cfg)?;

        let mut tree = Tree {
            root,
            nodes,
            expanded: HashSet::new(),
            filter: String::new(),
            cache_dirty: true,
            cached_rows: Vec::new(),
        };
        // Expand the top-level entries by default for a useful initial view.
        tree.expand_top_level();
        Ok(tree)
    }

    /// Expand all directories that sit directly under the root.
    fn expand_top_level(&mut self) {
        let to_expand: Vec<PathBuf> = self
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Dir && n.depth == 0)
            .map(|n| n.path.clone())
            .collect();
        for path in to_expand {
            self.expanded.insert(path);
        }
        self.cache_dirty = true;
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    pub fn toggle(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_path_buf());
        }
        self.cache_dirty = true;
    }

    pub fn expand(&mut self, path: &Path) {
        if self.expanded.insert(path.to_path_buf()) {
            self.cache_dirty = true;
        }
    }

    pub fn collapse(&mut self, path: &Path) {
        if self.expanded.remove(path) {
            self.cache_dirty = true;
        }
    }

    pub fn expand_all(&mut self) {
        for n in &self.nodes {
            if n.kind == NodeKind::Dir {
                self.expanded.insert(n.path.clone());
            }
        }
        self.cache_dirty = true;
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.cache_dirty = true;
    }

    pub fn set_filter(&mut self, filter: &str) {
        if self.filter != filter {
            self.filter = filter.to_string();
            self.cache_dirty = true;
            // Auto-expand parent directories of matches when filtering.
            if !self.filter.is_empty() {
                let f_lower = self.filter.to_lowercase();
                let matching_dirs: Vec<PathBuf> = self
                    .nodes
                    .iter()
                    .filter(|n| {
                        n.kind == NodeKind::File && n.name.to_lowercase().contains(&f_lower)
                    })
                    .flat_map(|n| ancestors_within_root(&n.path, &self.root))
                    .collect();
                for d in matching_dirs {
                    self.expanded.insert(d);
                }
            }
        }
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Compute the visible rows considering expansion state and active filter.
    pub fn visible_rows(&mut self) -> &[Row] {
        if !self.cache_dirty {
            return &self.cached_rows;
        }
        let mut rows = Vec::new();
        let f_lower = self.filter.to_lowercase();

        // Walk in path-sorted order; only emit nodes whose ancestor chain
        // (from root) is fully expanded.
        for node in &self.nodes {
            // Check that every ancestor directory between root and this node
            // is currently expanded.
            let mut ancestor_visible = true;
            for ancestor in ancestors_within_root(&node.path, &self.root) {
                if !self.expanded.contains(&ancestor) {
                    ancestor_visible = false;
                    break;
                }
            }
            if !ancestor_visible {
                continue;
            }

            let matches = if f_lower.is_empty() {
                true
            } else {
                match node.kind {
                    NodeKind::File => node.name.to_lowercase().contains(&f_lower),
                    NodeKind::Dir => {
                        // Directory matches if any descendant file matches.
                        self.nodes.iter().any(|n| {
                            n.kind == NodeKind::File
                                && n.path.starts_with(&node.path)
                                && n.name.to_lowercase().contains(&f_lower)
                        })
                    }
                }
            };

            // When a filter is active, hide non-matching rows entirely.
            if !f_lower.is_empty() && !matches {
                continue;
            }

            rows.push(Row {
                path: node.path.clone(),
                depth: node.depth,
                kind: node.kind.clone(),
                expanded: self.expanded.contains(&node.path),
                name: node.name.clone(),
                matches,
            });
        }

        self.cached_rows = rows;
        self.cache_dirty = false;
        &self.cached_rows
    }

    /// Number of markdown files found across the entire tree.
    pub fn file_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::File)
            .count()
    }

    /// Number of markdown files matching the current filter.
    pub fn match_count(&self) -> usize {
        if self.filter.is_empty() {
            return self.file_count();
        }
        let f = self.filter.to_lowercase();
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::File && n.name.to_lowercase().contains(&f))
            .count()
    }
}

/// Return every ancestor of `path` that sits *strictly between* `root` and
/// `path` (i.e. excludes the root itself and `path` itself). Ordered from
/// closest-to-root to closest-to-path.
fn ancestors_within_root(path: &Path, root: &Path) -> Vec<PathBuf> {
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let components: Vec<_> = rel.components().collect();
    if components.len() <= 1 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = root.to_path_buf();
    for c in &components[..components.len() - 1] {
        cur = cur.join(c);
        out.push(cur.clone());
    }
    out
}

/// Walk the directory and collect markdown files plus their ancestor
/// directories (only those that contain at least one markdown file).
fn walk_markdown(root: &Path, cfg: &TreeConfig) -> anyhow::Result<Vec<Node>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!cfg.show_hidden)
        .git_ignore(cfg.respect_gitignore)
        .git_exclude(cfg.respect_gitignore)
        .git_global(cfg.respect_gitignore)
        .ignore(cfg.respect_gitignore)
        .parents(cfg.respect_gitignore)
        .follow_links(false);
    if let Some(d) = cfg.max_depth {
        builder.max_depth(Some(d));
    }
    let walker = builder.build();

    let mut files: Vec<PathBuf> = Vec::new();
    for result in walker {
        match result {
            Ok(entry) => {
                if entry.depth() == 0 {
                    continue;
                }
                let ft = match entry.file_type() {
                    Some(ft) => ft,
                    None => continue,
                };
                if !ft.is_file() {
                    continue;
                }
                let path = entry.path();
                if is_markdown(path) {
                    files.push(path.to_path_buf());
                }
            }
            Err(_) => {
                // Permission errors and similar: ignore individual entries.
                continue;
            }
        }
    }

    files.sort();

    // Build the set of ancestor directories under root for each file.
    let mut dir_set: HashSet<PathBuf> = HashSet::new();
    for file in &files {
        for anc in ancestors_within_root(file, root) {
            dir_set.insert(anc);
        }
    }
    let mut dirs: Vec<PathBuf> = dir_set.into_iter().collect();
    dirs.sort();

    let mut nodes: Vec<Node> = Vec::with_capacity(files.len() + dirs.len());
    for d in &dirs {
        let depth = depth_of(d, root);
        nodes.push(Node {
            path: d.clone(),
            name: d
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.display().to_string()),
            kind: NodeKind::Dir,
            depth,
            children: Vec::new(),
        });
    }
    for f in &files {
        let depth = depth_of(f, root);
        nodes.push(Node {
            path: f.clone(),
            name: f
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| f.display().to_string()),
            kind: NodeKind::File,
            depth,
            children: Vec::new(),
        });
    }
    // Sort nodes by path so directories naturally appear before their
    // children. With the path-sorted order, depth-based filtering of visible
    // rows is straightforward.
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(nodes)
}

fn depth_of(path: &Path, root: &Path) -> usize {
    match path.strip_prefix(root) {
        Ok(rel) => rel.components().count().saturating_sub(1),
        Err(_) => 0,
    }
}

fn is_markdown(path: &Path) -> bool {
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => {
            let lower = ext.to_ascii_lowercase();
            MD_EXTENSIONS.iter().any(|e| *e == lower)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempdir_for_tests::TempDir;

    mod tempdir_for_tests {
        use std::env;
        use std::fs;
        use std::path::{Path, PathBuf};

        pub struct TempDir {
            path: PathBuf,
        }

        impl TempDir {
            pub fn new(prefix: &str) -> std::io::Result<Self> {
                let mut p = env::temp_dir();
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                p.push(format!("mdmux-{}-{}", prefix, stamp));
                fs::create_dir_all(&p)?;
                Ok(Self { path: p })
            }
            pub fn path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, "# stub").unwrap();
    }

    #[test]
    fn finds_markdown_files_recursively() {
        let td = TempDir::new("finds").unwrap();
        let r = td.path();
        touch(&r.join("a.md"));
        touch(&r.join("sub/b.md"));
        touch(&r.join("sub/nested/c.markdown"));
        touch(&r.join("not_md.txt"));

        let tree = Tree::build(r, &TreeConfig::default()).unwrap();
        assert_eq!(tree.file_count(), 3, "expected 3 markdown files");
    }

    #[test]
    fn ignores_non_markdown_extensions() {
        let td = TempDir::new("nonmd").unwrap();
        let r = td.path();
        touch(&r.join("a.txt"));
        touch(&r.join("b.rs"));
        let tree = Tree::build(r, &TreeConfig::default()).unwrap();
        assert_eq!(tree.file_count(), 0);
        assert!(tree.is_empty());
    }

    #[test]
    fn ancestors_within_root_works() {
        let root = Path::new("/r");
        let path = Path::new("/r/a/b/c.md");
        let got = ancestors_within_root(path, root);
        assert_eq!(got, vec![PathBuf::from("/r/a"), PathBuf::from("/r/a/b")]);
    }

    #[test]
    fn ancestors_returns_empty_for_top_level_file() {
        let root = Path::new("/r");
        let path = Path::new("/r/a.md");
        let got = ancestors_within_root(path, root);
        assert!(got.is_empty());
    }

    #[test]
    fn top_level_dirs_are_expanded_by_default() {
        let td = TempDir::new("expanded").unwrap();
        let r = td.path();
        touch(&r.join("top.md"));
        touch(&r.join("sub/inside.md"));
        touch(&r.join("sub/deep/nested.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        let rows = tree.visible_rows();
        // We should see: top.md, sub/, sub/inside.md, sub/deep/ (collapsed)
        // -- but NOT sub/deep/nested.md because sub/deep wasn't expanded.
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"top.md"));
        assert!(names.contains(&"sub"));
        assert!(names.contains(&"inside.md"));
        assert!(names.contains(&"deep"));
        assert!(
            !names.contains(&"nested.md"),
            "deep wasn't expanded, so nested.md should be hidden"
        );
    }

    #[test]
    fn toggle_expand_and_collapse() {
        let td = TempDir::new("toggle").unwrap();
        let r = td.path();
        touch(&r.join("sub/deep/nested.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        let deep = r.canonicalize().unwrap().join("sub").join("deep");
        assert!(!tree.is_expanded(&deep));
        tree.toggle(&deep);
        assert!(tree.is_expanded(&deep));
        let rows = tree.visible_rows();
        assert!(rows.iter().any(|r| r.name == "nested.md"));
        tree.toggle(&deep);
        assert!(!tree.is_expanded(&deep));
        let rows = tree.visible_rows();
        assert!(!rows.iter().any(|r| r.name == "nested.md"));
    }

    #[test]
    fn expand_all_makes_everything_visible() {
        let td = TempDir::new("expand_all").unwrap();
        let r = td.path();
        touch(&r.join("sub/deep/very/nested.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        tree.expand_all();
        let rows = tree.visible_rows();
        assert!(rows.iter().any(|r| r.name == "nested.md"));
    }

    #[test]
    fn collapse_all_keeps_only_top_level() {
        let td = TempDir::new("collapse_all").unwrap();
        let r = td.path();
        touch(&r.join("top.md"));
        touch(&r.join("sub/inside.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        tree.expand_all();
        tree.collapse_all();
        let rows = tree.visible_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"top.md"));
        assert!(names.contains(&"sub"));
        assert!(!names.contains(&"inside.md"));
    }

    #[test]
    fn filter_hides_non_matching_files() {
        let td = TempDir::new("filter").unwrap();
        let r = td.path();
        touch(&r.join("alpha.md"));
        touch(&r.join("beta.md"));
        touch(&r.join("sub/gamma.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        tree.set_filter("alpha");
        let rows = tree.visible_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"alpha.md"));
        assert!(!names.contains(&"beta.md"));
        assert!(!names.contains(&"gamma.md"));
    }

    #[test]
    fn filter_auto_expands_matching_subtrees() {
        let td = TempDir::new("filter_expand").unwrap();
        let r = td.path();
        touch(&r.join("sub/deep/match.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        tree.collapse_all();
        tree.set_filter("match");
        let rows = tree.visible_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"match.md"),
            "match.md should be visible after filter"
        );
    }

    #[test]
    fn empty_filter_restores_full_tree() {
        let td = TempDir::new("filter_clear").unwrap();
        let r = td.path();
        touch(&r.join("alpha.md"));
        touch(&r.join("beta.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        tree.set_filter("alpha");
        tree.set_filter("");
        let rows = tree.visible_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"alpha.md"));
        assert!(names.contains(&"beta.md"));
    }

    #[test]
    fn match_count_reflects_filter() {
        let td = TempDir::new("matchcount").unwrap();
        let r = td.path();
        touch(&r.join("alpha.md"));
        touch(&r.join("beta.md"));
        touch(&r.join("alpha2.md"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        assert_eq!(tree.match_count(), 3);
        tree.set_filter("alpha");
        assert_eq!(tree.match_count(), 2);
    }

    #[test]
    fn directories_without_markdown_are_hidden() {
        let td = TempDir::new("nomd_dir").unwrap();
        let r = td.path();
        touch(&r.join("with_md/file.md"));
        fs::create_dir_all(r.join("no_md/empty")).unwrap();
        touch(&r.join("no_md/other.txt"));

        let mut tree = Tree::build(r, &TreeConfig::default()).unwrap();
        let rows = tree.visible_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"with_md"));
        assert!(
            !names.contains(&"no_md"),
            "directory with no markdown should be hidden"
        );
    }
}
