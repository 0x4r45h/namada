use std::path::PathBuf;

use color_eyre::eyre::Result;
use color_eyre::owo_colors::OwoColorize;
use namada_apps::node::ledger::shell::testing::client::run;
use namada_apps::node::ledger::shell::testing::utils::{Bin, CapturedOutput};
use namada_sdk::masp::fs::FsShieldedUtils;
use test_log::test;

use super::setup;
use crate::e2e::setup::constants::{AA_PAYMENT_ADDRESS, AA_VIEWING_KEY, AB_PAYMENT_ADDRESS, AB_VIEWING_KEY, AC_PAYMENT_ADDRESS, AC_VIEWING_KEY, ALBERT, A_SPENDING_KEY, BB_PAYMENT_ADDRESS, BERTHA, BTC, B_SPENDING_KEY, CHRISTEL, ETH, MASP, NAM, BERTHA_KEY, ALBERT_KEY, CHRISTEL_KEY};

/// In this test we verify that users of the MASP receive the correct rewards
/// for leaving their assets in the pool for varying periods of time.
#[test]
fn masp_incentives() -> Result<()> {
    // This address doesn't matter for tests. But an argument is required.
    let validator_one_rpc = "127.0.0.1:26567";
    // Download the shielded pool parameters before starting node
    let _ = FsShieldedUtils::new(PathBuf::new());
    // Lengthen epoch to ensure that a transaction can be constructed and
    // submitted within the same block. Necessary to ensure that conversion is
    // not invalidated.
    let (mut node, _services) = setup::setup()?;
    // Wait till epoch boundary
    node.next_epoch();
    // Send 20 BTC from Albert to PA
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            ALBERT,
            "--target",
            AA_PAYMENT_ADDRESS,
            "--token",
            BTC,
            "--amount",
            "20",
            "--node",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // Assert BTC balance at VK(A) is 20
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                BTC,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("btc: 20"));

    // Assert NAM balance at VK(A) is 0
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("No shielded nam balance found"));

    // Wait till epoch boundary
    node.next_epoch();

    // Assert BTC balance at VK(A) is 20
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                BTC,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("btc: 20"));

    // Assert NAM balance at VK(A) is 20*BTC_reward*(epoch_1-epoch_0)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });

    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 22.74"));

    // Assert NAM balance at MASP pool is 20*BTC_reward*(epoch_1-epoch_0)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                MASP,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 22.74"));

    // Wait till epoch boundary
    node.next_epoch();

    // Assert BTC balance at VK(A) is 20
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                BTC,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("btc: 20"));

    // Assert NAM balance at VK(A) is 20*BTC_reward*(epoch_2-epoch_0)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 90.96"));

    // Assert NAM balance at MASP pool is 20*BTC_reward*(epoch_2-epoch_0)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                MASP,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 90.96"));

    // Wait till epoch boundary
    node.next_epoch();

    // Send 10 ETH from Albert to PA(B)
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            ALBERT,
            "--target",
            AB_PAYMENT_ADDRESS,
            "--token",
            ETH,
            "--amount",
            "10",
            "--node",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // Assert ETH balance at VK(B) is 10
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                ETH,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("eth: 10"));

    // Assert NAM balance at VK(B) is 0
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("No shielded nam balance found"));

    // Wait till epoch boundary
    node.next_epoch();

    // Assert ETH balance at VK(B) is 10
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                ETH,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("eth: 10"));

    // Assert NAM balance at VK(B) is 10*ETH_reward*(epoch_4-epoch_3)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 22.71432"));

    // Assert NAM balance at MASP pool is
    // 20*BTC_reward*(epoch_4-epoch_0)+10*ETH_reward*(epoch_4-epoch_3)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                MASP,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 386.46336"));

    // Wait till epoch boundary
    node.next_epoch();

    // Send 10 ETH from SK(B) to Christel
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            B_SPENDING_KEY,
            "--target",
            CHRISTEL,
            "--token",
            ETH,
            "--amount",
            "10",
            "--signing-keys",
            BERTHA_KEY,
            "--node",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // Assert ETH balance at VK(B) is 0
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                ETH,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("No shielded eth balance found"));

    node.next_epoch();

    // Assert NAM balance at VK(B) is 10*ETH_reward*(ep-epoch_3)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 86.60024"));

    node.next_epoch();
    // Assert NAM balance at MASP pool is
    // 20*BTC_reward*(epoch_5-epoch_0)+10*ETH_reward*(epoch_5-epoch_3)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                MASP,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 1180.41525"));

    // Wait till epoch boundary
    node.next_epoch();

    // Send 20 BTC from SK(A) to Christel
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            A_SPENDING_KEY,
            "--target",
            CHRISTEL,
            "--token",
            BTC,
            "--amount",
            "20",
            "--signing-keys",
            ALBERT_KEY,
            "--node",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // Assert BTC balance at VK(A) is 0
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                BTC,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("No shielded btc balance found"));

    // Assert NAM balance at VK(A) is 20*BTC_reward*(epoch_6-epoch_0)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 1407.16324"));

    // Assert NAM balance at MASP pool is
    // 20*BTC_reward*(epoch_6-epoch_0)+20*ETH_reward*(epoch_5-epoch_3)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                MASP,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 1520.37191"));

    // Wait till epoch boundary
    node.next_epoch();

    // Assert NAM balance at VK(A) is 20*BTC_reward*(epoch_6-epoch_0)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 1573.18"));

    // Assert NAM balance at VK(B) is 10*ETH_reward*(epoch_5-epoch_3)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 126.565"));

    // Assert NAM balance at MASP pool is
    // 20*BTC_reward*(epoch_6-epoch_0)+10*ETH_reward*(epoch_5-epoch_3)
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                MASP,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 1699.745"));

    // Wait till epoch boundary to prevent conversion expiry during transaction
    // construction
    node.next_epoch();

    // Send 10*ETH_reward*(epoch_5-epoch_3) NAM from SK(B) to Christel
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            B_SPENDING_KEY,
            "--target",
            CHRISTEL,
            "--token",
            NAM,
            "--amount",
            "141.49967",
            "--signing-keys",
            BERTHA_KEY,
            "--node",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // Wait till epoch boundary
    node.next_epoch();

    // Send 20*BTC_reward*(epoch_6-epoch_0) NAM from SK(A) to Bertha
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            A_SPENDING_KEY,
            "--target",
            BERTHA,
            "--token",
            NAM,
            "--amount",
            "1980.356",
            "--signing-keys",
            ALBERT_KEY,
            "--node",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // Assert NAM balance at VK(A) is 0
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("No shielded nam balance found"));

    // Assert NAM balance at VK(B) is 0
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("No shielded nam balance found"));

    // Assert NAM balance at MASP pool is 0
    let captured = CapturedOutput::of(|| {
        run(
            &node,
            Bin::Client,
            vec![
                "balance",
                "--owner",
                MASP,
                "--token",
                NAM,
                "--node",
                validator_one_rpc,
            ],
        )
    });
    assert!(captured.result.is_ok());
    assert!(captured.contains("nam: 0"));

    Ok(())
}

/// In this test we:
/// 1. Run the ledger node
/// 2. Assert PPA(C) cannot be recognized by incorrect viewing key
/// 3. Assert PPA(C) has not transaction pinned to it
/// 4. Send 20 BTC from Albert to PPA(C)
/// 5. Assert PPA(C) has the 20 BTC transaction pinned to it
#[test]
fn masp_pinned_txs() -> Result<()> {
    // This address doesn't matter for tests. But an argument is required.
    let validator_one_rpc = "127.0.0.1:26567";
    // Download the shielded pool parameters before starting node
    let _ = FsShieldedUtils::new(PathBuf::new());

    let (mut node, _services) = setup::setup()?;
    // Wait till epoch boundary
    let _ep0 = node.next_epoch();

    // Assert PPA(C) cannot be recognized by incorrect viewing key
    let captured =
        CapturedOutput::with_input(AB_VIEWING_KEY.into()).run(|| {
            run(
                &node,
                Bin::Client,
                vec![
                    "balance",
                    "--owner",
                    AC_PAYMENT_ADDRESS,
                    "--token",
                    BTC,
                    "--node",
                    validator_one_rpc,
                ],
            )
        });
    assert!(captured.result.is_ok());
    assert!(
        captured.contains("Supplied viewing key cannot decode transactions to")
    );

    // Assert PPA(C) has no transaction pinned to it
    let captured =
        CapturedOutput::with_input(AC_VIEWING_KEY.into()).run(|| {
            run(
                &node,
                Bin::Client,
                vec![
                    "balance",
                    "--owner",
                    AC_PAYMENT_ADDRESS,
                    "--token",
                    BTC,
                    "--node",
                    validator_one_rpc,
                ],
            )
        });
    assert!(captured.result.is_ok());
    assert!(captured.contains("has not yet been consumed"));

    // Wait till epoch boundary
    let _ep1 = node.next_epoch();

    // Send 20 BTC from Albert to PPA(C)
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            ALBERT,
            "--target",
            AC_PAYMENT_ADDRESS,
            "--token",
            BTC,
            "--amount",
            "20",
            "--node",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // Wait till epoch boundary
    // This makes it more consistent for some reason?
    let _ep2 = node.next_epoch();

    // Assert PPA(C) has the 20 BTC transaction pinned to it
    let captured =
        CapturedOutput::with_input(AC_VIEWING_KEY.into()).run(|| {
            run(
                &node,
                Bin::Client,
                vec![
                    "balance",
                    "--owner",
                    AC_PAYMENT_ADDRESS,
                    "--token",
                    BTC,
                    "--node",
                    validator_one_rpc,
                ],
            )
        });
    assert!(captured.result.is_ok());
    assert!(captured.contains("Received 20 btc"));

    // Assert PPA(C) has no NAM pinned to it
    let captured =
        CapturedOutput::with_input(AC_VIEWING_KEY.into()).run(|| {
            run(
                &node,
                Bin::Client,
                vec![
                    "balance",
                    "--owner",
                    AC_PAYMENT_ADDRESS,
                    "--token",
                    NAM,
                    "--node",
                    validator_one_rpc,
                ],
            )
        });
    assert!(captured.result.is_ok());
    assert!(captured.contains("Received no shielded nam"));

    // Wait till epoch boundary
    let _ep1 = node.next_epoch();

    // Assert PPA(C) does not NAM pinned to it on epoch boundary
    let captured =
        CapturedOutput::with_input(AC_VIEWING_KEY.into()).run(|| {
            run(
                &node,
                Bin::Client,
                vec![
                    "balance",
                    "--owner",
                    AC_PAYMENT_ADDRESS,
                    "--token",
                    NAM,
                    "--node",
                    validator_one_rpc,
                ],
            )
        });
    assert!(captured.result.is_ok());
    assert!(captured.contains("Received no shielded nam"));
    Ok(())
}

/// In this test we:
/// 1. Run the ledger node
/// 2. Attempt to spend 10 BTC at SK(A) to PA(B)
/// 3. Attempt to spend 15 BTC at SK(A) to Bertha
/// 4. Send 20 BTC from Albert to PA(A)
/// 5. Attempt to spend 10 ETH at SK(A) to PA(B)
/// 6. Spend 7 BTC at SK(A) to PA(B)
/// 7. Spend 7 BTC at SK(A) to PA(B)
/// 8. Attempt to spend 7 BTC at SK(A) to PA(B)
/// 9. Spend 6 BTC at SK(A) to PA(B)
/// 10. Assert BTC balance at VK(A) is 0
/// 11. Assert ETH balance at VK(A) is 0
/// 12. Assert balance at VK(B) is 10 BTC
/// 13. Send 10 BTC from SK(B) to Bertha
#[test]
fn masp_txs_and_queries() -> Result<()> {
    // Uncomment for better debugging
    // let _log_guard = namada_apps::logging::init_from_env_or(
    //     tracing::level_filters::LevelFilter::INFO,
    // )?;
    // This address doesn't matter for tests. But an argument is required.
    let validator_one_rpc = "127.0.0.1:26567";
    // Download the shielded pool parameters before starting node
    let _ = FsShieldedUtils::new(PathBuf::new());

    enum Response {
        Ok(&'static str),
        Err(&'static str),
    }

    let (mut node, _services) = setup::setup()?;
    _ = node.next_epoch();
    let txs_args = vec![
        // 0. Attempt to spend 10 BTC at SK(A) to PA(B)
        (
            vec![
                "transfer",
                "--source",
                A_SPENDING_KEY,
                "--target",
                AB_PAYMENT_ADDRESS,
                "--token",
                BTC,
                "--amount",
                "10",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Err(""),
        ),
        // 1. Attempt to spend 15 BTC at SK(A) to Bertha
        (
            vec![
                "transfer",
                "--source",
                A_SPENDING_KEY,
                "--target",
                BERTHA,
                "--token",
                BTC,
                "--amount",
                "15",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Err(""),
        ),
        // 2. Send 20 BTC from Albert to PA(A)
        (
            vec![
                "transfer",
                "--source",
                ALBERT,
                "--target",
                AA_PAYMENT_ADDRESS,
                "--token",
                BTC,
                "--amount",
                "20",
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("Transaction is valid"),
        ),
        // 3. Attempt to spend 10 ETH at SK(A) to PA(B)
        (
            vec![
                "transfer",
                "--source",
                A_SPENDING_KEY,
                "--target",
                AB_PAYMENT_ADDRESS,
                "--token",
                ETH,
                "--amount",
                "10",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Err(""),
        ),
        // 4. Spend 7 BTC at SK(A) to PA(B)
        (
            vec![
                "transfer",
                "--source",
                A_SPENDING_KEY,
                "--target",
                AB_PAYMENT_ADDRESS,
                "--token",
                BTC,
                "--amount",
                "7",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("Transaction is valid"),
        ),
        // 5. Spend 7 BTC at SK(A) to PA(B)
        (
            vec![
                "transfer",
                "--source",
                A_SPENDING_KEY,
                "--target",
                BB_PAYMENT_ADDRESS,
                "--token",
                BTC,
                "--amount",
                "7",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("Transaction is valid"),
        ),
        // 6. Attempt to spend 7 BTC at SK(A) to PA(B)
        (
            vec![
                "transfer",
                "--source",
                A_SPENDING_KEY,
                "--target",
                BB_PAYMENT_ADDRESS,
                "--token",
                BTC,
                "--amount",
                "7",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Err(""),
        ),
        // 7. Spend 6 BTC at SK(A) to PA(B)
        (
            vec![
                "transfer",
                "--source",
                A_SPENDING_KEY,
                "--target",
                BB_PAYMENT_ADDRESS,
                "--token",
                BTC,
                "--amount",
                "6",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("Transaction is valid"),
        ),
        // 8. Assert BTC balance at VK(A) is 0
        (
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                BTC,
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("No shielded btc balance found"),
        ),
        // 9. Assert ETH balance at VK(A) is 0
        (
            vec![
                "balance",
                "--owner",
                AA_VIEWING_KEY,
                "--token",
                ETH,
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("No shielded eth balance found"),
        ),
        // 10. Assert balance at VK(B) is 20 BTC
        (
            vec![
                "balance",
                "--owner",
                AB_VIEWING_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("btc : 20"),
        ),
        // 11. Send 20 BTC from SK(B) to Bertha
        (
            vec![
                "transfer",
                "--source",
                B_SPENDING_KEY,
                "--target",
                BERTHA,
                "--token",
                BTC,
                "--amount",
                "20",
                "--gas-payer",
                CHRISTEL_KEY,
                "--node",
                validator_one_rpc,
            ],
            Response::Ok("Transaction is valid"),
        ),
    ];

    for (tx_args, tx_result) in &txs_args {
        // We ensure transfers don't cross epoch boundaries.
        if tx_args[0] == "transfer" {
            node.next_epoch();
        }
        for &dry_run in &[true, false] {
            let tx_args = if dry_run && tx_args[0] == "transfer" {
                vec![tx_args.clone(), vec!["--dry-run"]].concat()
            } else {
                tx_args.clone()
            };
            println!(
                "{}: {:?}\n\n",
                "Running".green().underline(),
                tx_args.join(" ").yellow().underline()
            );
            let captured =
                CapturedOutput::of(|| run(&node, Bin::Client, tx_args.clone()));
            match tx_result {
                Response::Ok("Transaction is valid") => {
                    assert!(
                        captured.result.is_ok(),
                        "{:?} failed with result {:?}.\n Unread output: {}",
                        tx_args,
                        captured.result,
                        captured.output,
                    );
                    if !dry_run {
                        node.assert_success();
                    } else {
                        assert!(
                            captured.contains("Transaction is valid"),
                            "{:?} failed to contain needle 'Transaction is \
                             valid',\nGot output '{}'",
                            tx_args,
                            captured.output
                        );
                    }
                }
                Response::Ok(out) => {
                    assert!(
                        captured.result.is_ok(),
                        "{:?} failed with result {:?}.\n Unread output: {}",
                        tx_args,
                        captured.result,
                        captured.output,
                    );
                    assert!(
                        captured.contains(out),
                        "{:?} failed to contain needle '{}',\nGot output '{}'",
                        tx_args,
                        out,
                        captured.output
                    );
                }
                Response::Err(msg) => {
                    assert!(
                        captured.result.is_err(),
                        "{:?} unexpectedly succeeded",
                        tx_args
                    );
                    assert!(
                        captured.contains(msg),
                        "{:?} failed to contain needle {},\nGot output {}",
                        tx_args,
                        msg,
                        captured.output
                    );
                }
            }
        }
    }

    Ok(())
}

/// Test the unshielding tx attached to a wrapper:
///
/// 1. Shield some tokens to reduce the unshielded balance
/// 2. Submit a new wrapper with a valid unshielding tx and assert
/// success
/// 3. Submit a new wrapper with an invalid unshielding tx and assert the
/// failure
#[test]
fn wrapper_fee_unshielding() -> Result<()> {
    // This address doesn't matter for tests. But an argument is required.
    let validator_one_rpc = "127.0.0.1:26567";
    // Download the shielded pool parameters before starting node
    let _ = FsShieldedUtils::new(PathBuf::new());
    // Lengthen epoch to ensure that a transaction can be constructed and
    // submitted within the same block. Necessary to ensure that conversion is
    // not invalidated.
    let (mut node, _services) = setup::setup()?;
    _ = node.next_epoch();

    // 1. Shield some tokens
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            ALBERT,
            "--target",
            AA_PAYMENT_ADDRESS,
            "--token",
            NAM,
            "--amount",
            "500000",
            "--gas-price",
            "30", // Reduce the balance of the fee payer artificially
            "--gas-limit",
            "20000",
            "--ledger-address",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    _ = node.next_epoch();
    // 2. Valid unshielding
    run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            ALBERT,
            "--target",
            BERTHA,
            "--token",
            NAM,
            "--amount",
            "1",
            "--gas-limit",
            "20000",
            "--gas-spending-key",
            A_SPENDING_KEY,
            "--ledger-address",
            validator_one_rpc,
        ],
    )?;
    node.assert_success();

    // 3. Invalid unshielding
    // TODO: this test shall panic because of the panic in the sdk. Once the
    // panics are removed from there, this test can be updated
    let tx_run = run(
        &node,
        Bin::Client,
        vec![
            "transfer",
            "--source",
            ALBERT,
            "--target",
            BERTHA,
            "--token",
            NAM,
            "--amount",
            "1",
            "--gas-price",
            "1000",
            "--gas-spending-key",
            B_SPENDING_KEY,
            "--ledger-address",
            validator_one_rpc,
            "--force",
        ],
    )
    .is_err();

    assert!(tx_run);
    Ok(())
}
