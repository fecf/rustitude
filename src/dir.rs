use std::{fs, io::Error, path::Path};

pub fn get_directory_size_recursive(
  path: &Path,
  cb: &mut impl FnMut(&str, &str, bool, u64) -> Result<bool, Error>,
) -> Result<(u64, bool), Error> {
  fn get_directory_size_recursive_impl(
      canceled: &mut bool,
      path: &Path,
      cb: &mut impl FnMut(&str, &str, bool, u64) -> Result<bool, Error>,
  ) -> Result<(u64, bool), Error> {
      let mut total: u64 = 0;

      let dir = fs::read_dir(path)?;
      for entry in dir {
          let entry = entry?;
          let metadata = entry.metadata()?;
          let is_dir = metadata.is_dir();
          let size = if is_dir {
              let result =
                  get_directory_size_recursive_impl(canceled, entry.path().as_path(), cb)?;
              *canceled = !result.1;
              if *canceled {
                  return Ok((total, false));
              } else {
                  result.0
              }
          } else {
              metadata.len()
          };
          total += size;

          *canceled = !cb(
              path.to_str().unwrap(),
              entry.path().to_str().unwrap(),
              is_dir,
              size,
          )?;
          if *canceled {
              return Ok((total, false));
          }
      }

      Ok((total, true))
  }

  let mut canceled = false;
  return get_directory_size_recursive_impl(&mut canceled, path, cb);
}
