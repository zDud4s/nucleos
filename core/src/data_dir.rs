//! placeholder, replaced in Step 3

#[cfg(test)]
mod tests {
    use super::*;

    /// `main.rs` used to match on the variable inline. The rule is the same one, now shared with the
    /// settings recovery CLI, so the two can never look at different databases.
    #[test]
    fn an_empty_data_dir_override_is_not_an_override() {
        let default = Path::new("default-dir");
        assert_eq!(data_dir_from(Some("elsewhere"), default), PathBuf::from("elsewhere"));
        assert_eq!(data_dir_from(Some(""), default), PathBuf::from("default-dir"));
        assert_eq!(data_dir_from(None, default), PathBuf::from("default-dir"));
    }

    #[test]
    fn the_database_is_a_file_directly_under_the_data_dir() {
        assert_eq!(DB_FILE, "nucleos.db");
        assert_eq!(db_path(Path::new("some-dir")), Path::new("some-dir").join("nucleos.db"));
    }

    /// Startup and the CLI must resolve the platform default from the SAME `ProjectDirs` triple.
    #[test]
    fn the_default_data_dir_is_the_project_dirs_local_data_dir() {
        let expected = directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
            .expect("this machine has a home directory")
            .data_local_dir()
            .to_path_buf();
        assert_eq!(default_data_dir(), Ok(expected));
    }
}
