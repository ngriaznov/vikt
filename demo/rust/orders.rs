//! The canonical cross-language demo, in Rust. Compare demo/java and
//! demo/python: same program, same expected tiering.

static mut RUNNING_TOTAL: f64 = 0.0;

pub fn process(prices: &[Option<f64>], tax_rate: f64, apply_tax: bool) -> f64 {
    println!("processing {} prices", prices.len());
    let unused = "this value goes nowhere";
    let mut inspected = 0u32;
    let mut subtotal = 0.0f64;
    for price in prices {
        let Some(p) = price else { continue };
        if *p < 0.0 {
            continue;
        }
        subtotal += p;
        inspected += 1;
    }
    let mut total = subtotal;
    if apply_tax {
        total = subtotal * (1.0 + tax_rate);
    }
    println!("inspected {} entries", inspected);
    let _ = unused;
    unsafe { RUNNING_TOTAL = total };
    total
}
