use core::num::ParseIntError;
use log;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::env;
use std::error;
use std::ffi::c_void;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::os::windows::prelude::OsStrExt;
use std::time::Instant;
use std::{thread, time};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::UI::Shell::{SHGetSpecialFolderPathW, CSIDL_MYPICTURES};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_SETDESKWALLPAPER,
};

const URL_DESKTOP: &str = "http://api.simpledesktops.com/v1/desktop_mobile/?format=json&limit=1";

#[derive(Clone, Debug)]
pub enum ApplicationError {
    DeserializationError { e: String },
    RequestError { e: String },
    ApiError { e: String },
    IoError { e: String },
    WindowsOSError { e: String },
    WrongEnvironmentVariable { e: String },
}

impl error::Error for ApplicationError {}

impl From<reqwest::Error> for ApplicationError {
    fn from(err: reqwest::Error) -> Self {
        ApplicationError::RequestError { e: err.to_string() }
    }
}

impl From<std::io::Error> for ApplicationError {
    fn from(err: std::io::Error) -> Self {
        ApplicationError::IoError { e: err.to_string() }
    }
}

impl From<serde_json::Error> for ApplicationError {
    fn from(err: serde_json::Error) -> Self {
        ApplicationError::DeserializationError { e: err.to_string() }
    }
}

impl From<ParseIntError> for ApplicationError {
    fn from(err: ParseIntError) -> Self {
        ApplicationError::WrongEnvironmentVariable { e: err.to_string() }
    }
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ApplicationError {:?}", self)
    }
}

pub type ApplicationResult<T> = std::result::Result<T, ApplicationError>;

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    limit: u32,
    next: Option<String>,
    offset: u32,
    previous: Option<String>,
    total_count: u32,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Creator {
    email: Option<String>,
    name: Option<String>,
    url: Option<String>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Object {
    creator: Creator,
    id: String,
    iphone_thumb: String,
    permalink: String,
    title: String,
    url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonWallpaperList {
    meta: Meta,
    objects: Vec<Object>,
}

struct SimpleWallpaper<'a> {
    pub total_count: u32,
    pub directory: &'a str,
}

impl<'a> SimpleWallpaper<'a> {
    pub async fn new(dir: &'a str) -> ApplicationResult<SimpleWallpaper<'a>> {
        match Self::get_wallpaper_list(0).await {
            Ok(wallpaper_list) => Ok(SimpleWallpaper {
                total_count: wallpaper_list.meta.total_count,
                directory: dir,
            }),
            Err(e) => Err(e),
        }
    }

    async fn get_wallpaper_list(offset: u32) -> ApplicationResult<JsonWallpaperList> {
        let text = reqwest::get(Self::get_url_for_offset(offset))
            .await?
            .text()
            .await?;
        serde_json::from_str::<JsonWallpaperList>(&text)
            .map_err(|ref e| ApplicationError::DeserializationError { e: e.to_string() })
    }

    fn get_url_for_offset(offset: u32) -> String {
        format!("{}&offset={}", URL_DESKTOP, offset)
    }

    pub async fn download_wallpaper(&self, number: u32, dir: &str) -> ApplicationResult<String> {
        let wallpaper_list = Self::get_wallpaper_list(number).await?;
        let sd_directory = String::from(dir) + "/" + self.directory + "/";
        fs::create_dir_all(&sd_directory)?;

        if wallpaper_list.objects.len() < 1 {
            Err(ApplicationError::ApiError {
                e: "The list of objects retrieved is less than 1".to_owned(),
            })
        } else {
            let wallpaper_filename = sd_directory + &wallpaper_list.objects[0].title + ".png";
            if !std::path::Path::new(&wallpaper_filename).exists() {
                let mut wallpaper_file = File::create(&wallpaper_filename)?;
                let bytes = reqwest::get(&wallpaper_list.objects[0].url)
                    .await?
                    .bytes()
                    .await?;
                wallpaper_file.write_all(&bytes)?;
                log::trace!("Downloaded wallpaper at '{}'", wallpaper_filename);
            }
            Ok(wallpaper_filename)
        }
    }
}

pub fn get_special_directory(csidl: i32) -> ApplicationResult<String> {
    let mut buffer = [0; MAX_PATH as usize];
    let result = unsafe { SHGetSpecialFolderPathW(HWND::default(), &mut buffer, csidl, false) };

    if result != BOOL(0) {
        Ok(String::from_utf16_lossy(&buffer)
            .trim_matches(char::from(0))
            .to_string())
    } else {
        Err(ApplicationError::WindowsOSError {
            e: format!(
                "SHGetSpecialFolderPathW failed: {}",
                std::io::Error::last_os_error()
            ),
        })
    }
}

fn get_image_path() -> ApplicationResult<String> {
    get_special_directory(CSIDL_MYPICTURES as _)
}

fn set_wallpaper(path: &str) -> ApplicationResult<()> {
    let mut path: Vec<u16> = OsStr::new(path).encode_wide().collect();
    // append null byte
    path.push(0);

    let successful = unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(path.as_ptr() as *mut c_void),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        ) != BOOL(0)
    };

    if successful {
        Ok(())
    } else {
        Err(ApplicationError::WindowsOSError {
            e: format!(
                "SystemParametersInfoW failed: {}",
                std::io::Error::last_os_error()
            ),
        })
    }
}

#[tokio::main]
async fn main() -> ApplicationResult<()> {
    pretty_env_logger::init();
    let mut sleep_time = time::Duration::from_secs(60 * 60);
    let check_time = time::Duration::from_secs(60);
    let default_download_directory = get_image_path()?;
    let download_directory =
        env::var("SIMPLE_DESKTOP_DIRECTORY").unwrap_or_else(|_| default_download_directory);

    let simple_wallpaper = SimpleWallpaper::new("SimpleDesktop").await?;
    let mut rng = rand::thread_rng();

    let mut staring = Instant::now();

    loop {
        if staring.elapsed() > sleep_time {
            let wallpaper_name = simple_wallpaper
                .download_wallpaper(
                    rng.gen_range(0, simple_wallpaper.total_count),
                    &download_directory,
                )
                .await?;
            set_wallpaper(&wallpaper_name)?;
            staring = Instant::now(); // reset the current time
        }

        if let Ok(timeout) = env::var("SIMPLE_DESKTOP_TIMEOUT") {
            let int_timeout = timeout.parse::<u64>()? * 60;
            sleep_time = time::Duration::from_secs(int_timeout);
            log::trace!("SimpleWallpaper: change timeout via env to {}", int_timeout);
        }
        thread::sleep(check_time);
    }
}
