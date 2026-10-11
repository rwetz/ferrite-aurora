//! Current NOAA SWPC feeds. RTSW records may arrive newest-first.
use serde_json::Value;

pub struct Space {
    pub kp: f32,
    pub time: String,
    pub history: Vec<(String, f32)>,
    pub speed: Option<f32>,
    pub density: Option<f32>,
    pub bz: Option<f32>,
    pub forecast_time: String,
    pub grid: Vec<u8>,
    pub notes: Vec<String>,
}
fn numeric(v: &Value) -> Option<f32> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite())
        .map(|n| n as f32)
}
pub fn kp(json: &str) -> Result<Vec<(String, f32)>, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "Invalid NOAA Kp response")?;
    let array = v.as_array().ok_or("Missing NOAA Kp readings")?;
    let mut values: Vec<_> = array
        .iter()
        .filter_map(|row| {
            let (time, value) = if row.is_object() {
                (row["time_tag"].as_str()?, numeric(&row["Kp"])?)
            } else {
                (row.get(0)?.as_str()?, numeric(row.get(1)?)?)
            };
            (0. ..=9.)
                .contains(&value)
                .then(|| (time.to_string(), value))
        })
        .collect();
    values.sort_by(|a, b| a.0.cmp(&b.0));
    if values.is_empty() {
        return Err("NOAA returned no valid Kp readings".into());
    }
    Ok(values)
}
fn wind(json: &str, key: &str) -> Option<f32> {
    let v: Value = serde_json::from_str(json).ok()?;
    v.as_array()?
        .iter()
        .filter(|r| {
            r["active"] == true
                && numeric(&r["overall_quality"]).unwrap_or(0.) == 0.
                && numeric(&r[key]).is_some_and(|n| n.abs() < 5000.)
        })
        .max_by_key(|r| r["time_tag"].as_str().unwrap_or_default())
        .and_then(|r| numeric(&r[key]))
}
pub fn ovation(json: &str) -> Result<(String, Vec<u8>), String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "Invalid NOAA aurora response")?;
    let coords = v["coordinates"]
        .as_array()
        .ok_or("Missing aurora model coordinates")?;
    let mut grid = vec![0; 360 * 181];
    let mut valid = 0;
    for c in coords {
        let Some(lon) = c.get(0).and_then(numeric) else {
            continue;
        };
        let Some(lat) = c.get(1).and_then(numeric) else {
            continue;
        };
        let Some(intensity) = c.get(2).and_then(numeric) else {
            continue;
        };
        if !(-90. ..=90.).contains(&lat) || !(0. ..=100.).contains(&intensity) {
            continue;
        }
        let x = lon.round().rem_euclid(360.) as usize;
        let y = (lat.round() + 90.) as usize;
        grid[y * 360 + x] = intensity.round() as u8;
        valid += 1;
    }
    if valid == 0 {
        return Err("Empty aurora model".into());
    }
    Ok((
        v["Forecast Time"]
            .as_str()
            .unwrap_or("Unknown model time")
            .into(),
        grid,
    ))
}
fn get(path: &str) -> Result<String, String> {
    let mut r = ureq::get(&format!("https://services.swpc.noaa.gov/{path}"))
        .header("User-Agent", "Ferrite-Aurora/0.1")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .call()
        .map_err(|e| format!("NOAA: {e}"))?;
    r.body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()
        .map_err(|_| "Couldn't read NOAA data".into())
}
pub fn fetch() -> Result<Space, String> {
    let (k, p, m, a) = std::thread::scope(|scope| {
        let k = scope.spawn(|| get("products/noaa-planetary-k-index.json"));
        let p = scope.spawn(|| get("json/rtsw/rtsw_wind_1m.json"));
        let m = scope.spawn(|| get("json/rtsw/rtsw_mag_1m.json"));
        let a = scope.spawn(|| get("json/ovation_aurora_latest.json"));
        (
            k.join().unwrap(),
            p.join().unwrap(),
            m.join().unwrap(),
            a.join().unwrap(),
        )
    });
    let mut history = kp(&k?)?;
    let (time, kp) = history.last().cloned().ok_or("No geomagnetic readings")?;
    if history.len() > 24 {
        history.drain(..history.len() - 24);
    }
    let mut notes = Vec::new();
    let (speed, density) = match p {
        Ok(s) => (wind(&s, "proton_speed"), wind(&s, "proton_density")),
        Err(e) => {
            notes.push(format!("Solar wind unavailable: {e}"));
            (None, None)
        }
    };
    let bz = match m {
        Ok(s) => wind(&s, "bz_gsm"),
        Err(e) => {
            notes.push(format!("Magnetic field unavailable: {e}"));
            None
        }
    };
    let (forecast_time, grid) = match a.and_then(|s| ovation(&s)) {
        Ok(v) => v,
        Err(e) => {
            notes.push(format!("Aurora model unavailable: {e}"));
            ("Unavailable".into(), Vec::new())
        }
    };
    Ok(Space {
        kp,
        time,
        history,
        speed,
        density,
        bz,
        forecast_time,
        grid,
        notes,
    })
}
pub fn coords(text: &str) -> Option<(f32, f32)> {
    let (a, b) = text.trim().split_once(',')?;
    let lat = a.trim().parse::<f32>().ok()?;
    let lon = b.trim().parse::<f32>().ok()?;
    ((-90. ..=90.).contains(&lat) && (-180. ..=180.).contains(&lon)).then_some((lat, lon))
}
pub fn intensity(grid: &[u8], lat: f32, lon: f32) -> Option<u8> {
    grid.get((lat.round() + 90.) as usize * 360 + lon.round().rem_euclid(360.) as usize)
        .copied()
}
pub fn globe(grid: &[u8], location: (f32, f32)) -> Vec<u8> {
    const N: usize = 480;
    let (latitude, longitude) = location;
    let marker_r = latitude.to_radians().cos();
    let marker_a = longitude.to_radians();
    let (mx, my) = (marker_r * marker_a.cos(), marker_r * marker_a.sin());
    let mut pixels = vec![0; N * N];
    for y in 0..N {
        for x in 0..N {
            let (u, v) = (
                (x as f32 / (N - 1) as f32 - 0.5) * 2.,
                (y as f32 / (N - 1) as f32 - 0.5) * 2.,
            );
            let r2 = u * u + v * v;
            if r2 > 1. {
                continue;
            }
            let latitude = (1. - r2).sqrt().asin().to_degrees();
            let longitude = v.atan2(u).to_degrees();
            let probability = intensity(grid, latitude, longitude).unwrap_or(0) as f32 / 100.;
            let graticule = latitude.rem_euclid(30.) < 0.8 || longitude.rem_euclid(30.) < 0.8;
            let marker = location.0 >= 0.
                && ((u - mx).abs() < 0.013 && (v - my).abs() < 0.065
                    || (v - my).abs() < 0.013 && (u - mx).abs() < 0.065);
            pixels[y * N + x] = if marker {
                255
            } else {
                ((0.04
                    + 0.09 * (1. - r2).sqrt()
                    + 0.7 * probability
                    + if graticule { 0.12 } else { 0. })
                .min(1.)
                    * 255.) as u8
            };
        }
    }
    pixels
}
pub fn storm(kp: f32) -> &'static str {
    if kp < 5. {
        "QUIET / UNSETTLED"
    } else if kp < 6. {
        "G1 MINOR"
    } else if kp < 7. {
        "G2 MODERATE"
    } else if kp < 8. {
        "G3 STRONG"
    } else if kp < 9. {
        "G4 SEVERE"
    } else {
        "G5 EXTREME"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_and_legacy_kp_are_sorted_and_validated() {
        let v=kp(r#"[{"time_tag":"2026-10-11","Kp":5.3},{"time_tag":"2026-10-10","Kp":2},{"time_tag":"bad","Kp":-9999}]"#).unwrap();
        assert_eq!(v.last().unwrap().1, 5.3);
        assert!(kp("[]").is_err());
        assert_eq!(
            kp(r#"[["time_tag","Kp"],["2026-10-11","3.33"]]"#).unwrap()[0].1,
            3.33
        );
    }
    #[test]
    fn rtsw_selects_latest_active_sample() {
        assert_eq!(
            wind(
                r#"[{"time_tag":"2026-10-11","active":true,"proton_speed":600},{"time_tag":"2026-10-10","active":true,"proton_speed":400}]"#,
                "proton_speed"
            ),
            Some(600.)
        );
    }
    #[test]
    fn globe_longitudes_and_locations_are_bounded() {
        let (_, grid) = ovation(r#"{"coordinates":[[264,47,25],[0,90,100]]}"#).unwrap();
        assert_eq!(intensity(&grid, 47., -96.), Some(25));
        assert!(coords("NaN,0").is_none());
        assert!(coords("91,0").is_none());
        assert_eq!(globe(&grid, (47., -96.)).len(), 480 * 480);
    }
}
