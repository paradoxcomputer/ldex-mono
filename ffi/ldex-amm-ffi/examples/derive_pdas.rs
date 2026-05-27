fn main() {
    use nssa_core::account::AccountId;
    use ldex_amm_ffi::{ldex_amm_pool_id, ldex_amm_vault_id, ldex_amm_lp_lock_id, ldex_amm_parse_account_id};
    let amm_v2_str = "4086f5f1f3bcc517073ddc1fdf3ea713bba6ff59e52276bf376fec68cbd86be8";
    let def_a_str = "Public/9wrHD1PNW6Z1B9uxmVV2F4uaididCTWGua1wSFtCJEW2";
    let def_b_str = "Public/3sc5NGghHnv6QW9Cq53uinwasPKz2TYBSJR4m7HSo2CB";
    let mut amm = [0u8;32]; let mut a = [0u8;32]; let mut b=[0u8;32];
    let mut pool=[0u8;32]; let mut va=[0u8;32]; let mut vb=[0u8;32]; let mut lock=[0u8;32];
    unsafe {
        let amm_c = std::ffi::CString::new(amm_v2_str).unwrap();
        let a_c = std::ffi::CString::new(def_a_str).unwrap();
        let b_c = std::ffi::CString::new(def_b_str).unwrap();
        ldex_amm_parse_account_id(amm_c.as_ptr(), amm.as_mut_ptr());
        ldex_amm_parse_account_id(a_c.as_ptr(), a.as_mut_ptr());
        ldex_amm_parse_account_id(b_c.as_ptr(), b.as_mut_ptr());
        ldex_amm_pool_id(amm.as_ptr(), a.as_ptr(), b.as_ptr(), 30, pool.as_mut_ptr());
        ldex_amm_vault_id(amm.as_ptr(), pool.as_ptr(), a.as_ptr(), va.as_mut_ptr());
        ldex_amm_vault_id(amm.as_ptr(), pool.as_ptr(), b.as_ptr(), vb.as_mut_ptr());
        ldex_amm_lp_lock_id(amm.as_ptr(), pool.as_ptr(), lock.as_mut_ptr());
    }
    println!("pool   = Public/{}", AccountId::new(pool));
    println!("vault_a= Public/{}", AccountId::new(va));
    println!("vault_b= Public/{}", AccountId::new(vb));
    println!("lp_lock= Public/{}", AccountId::new(lock));
}
