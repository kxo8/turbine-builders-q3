import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { AnchorCoreStaking } from "../target/types/anchor_core_staking";

import {
  ComputeBudgetProgram,
  SendTransactionError,
  SystemProgram,
  SYSVAR_CLOCK_PUBKEY,
} from "@solana/web3.js";

import {
  MPL_CORE_PROGRAM_ID,
  deserializeAssetV1,
} from "@metaplex-foundation/mpl-core";
import { publicKey, lamports } from "@metaplex-foundation/umi";
import { ASSOCIATED_TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { assert } from "chai";
const MILLISECONDS_PER_DAY = 86400000;
const REWARDS_BPS = 10000;
const FREEZE_PERIOD_IN_DAYS = 7;
const TIME_TRAVEL_IN_DAYS = 5;

describe("anchor-core-staking", () => {
  // Configure the client to use the local cluster.
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.anchorCoreStaking as Program<AnchorCoreStaking>;

  // Generate a keypair for the collection
  const collectionKeypair = anchor.web3.Keypair.generate();

  // Find the update authority for the collection (PDA)
  const updateAuthority = anchor.web3.PublicKey.findProgramAddressSync(
    [Buffer.from("update_authority"), collectionKeypair.publicKey.toBuffer()],
    program.programId
  )[0];

  // Generate a keypair for the nft asset
  const nftKeypair = anchor.web3.Keypair.generate();

  const burnNftKeypair = anchor.web3.Keypair.generate();

  // Find the config account (PDA)
  const config = anchor.web3.PublicKey.findProgramAddressSync(
    [Buffer.from("config"), collectionKeypair.publicKey.toBuffer()],
    program.programId
  )[0];

  // Find the rewards mint account (PDA)
  const rewardsMint = anchor.web3.PublicKey.findProgramAddressSync(
    [Buffer.from("rewards_mint"), config.toBuffer()],
    program.programId
  )[0];

  // Helper function to advance time with Surfpool 
  async function advanceTime(params: { absoluteEpoch?: number; absoluteSlot?: number; absoluteTimestamp?: number }): Promise<void> {
    const rpcResponse = await fetch(provider.connection.rpcEndpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "surfnet_timeTravel",
        params: [params],
      }),
    });

    const result = await rpcResponse.json() as { error?: any; result?: any };
    if (result.error) {
      throw new Error(`Time travel failed: ${JSON.stringify(result.error)}`);
    }
    
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }

  it("Create a collection", async () => {
    const collectionName = "Test Collection";
    const collectionUri = "https://example.com/collection";
    const tx = await program.methods.createCollection(collectionName, collectionUri)
    .accountsPartial({
      payer: provider.wallet.publicKey,
      collection: collectionKeypair.publicKey,
      updateAuthority,
      systemProgram: SystemProgram.programId,
      mplCoreProgram: MPL_CORE_PROGRAM_ID,
    })
    .signers([collectionKeypair])
    .rpc();
    console.log("\nYour transaction signature", tx);
    console.log("Collection address", collectionKeypair.publicKey.toBase58());
  });

  it("Mint an NFT", async () => {
    const nftName = "Test NFT";
    const nftUri = "https://example.com/nft";
    const tx = await program.methods.mintAsset(nftName, nftUri)
    .accountsPartial({
      user: provider.wallet.publicKey,
      asset: nftKeypair.publicKey,
      collection: collectionKeypair.publicKey,
      updateAuthority,
      systemProgram: SystemProgram.programId,
      mplCoreProgram: MPL_CORE_PROGRAM_ID,
    })
    .signers([nftKeypair])
    .rpc();
    console.log("\nYour transaction signature", tx);
    console.log("NFT address", nftKeypair.publicKey.toBase58());
  });

  it("Initialize Config", async () => {
    const tx = await program.methods.initialize(REWARDS_BPS, FREEZE_PERIOD_IN_DAYS)
    .accountsPartial({
      admin: provider.wallet.publicKey,
      collection: collectionKeypair.publicKey,
      updateAuthority,
      config,
      rewardsMint,
      systemProgram: SystemProgram.programId,
      tokenProgram: TOKEN_PROGRAM_ID,
    })
    .rpc();
    console.log("\nYour transaction signature", tx);
    console.log("Config address", config.toBase58());
    console.log("Rewards BPS", REWARDS_BPS);
    console.log("Freeze period in days", FREEZE_PERIOD_IN_DAYS);
    console.log("Rewards mint address", rewardsMint.toBase58());
  });

  it("Stake an NFT", async () => {
    const tx = await program.methods.stake()
    .accountsPartial({
      owner: provider.wallet.publicKey,
      updateAuthority,
      config,
      asset: nftKeypair.publicKey,
      collection: collectionKeypair.publicKey,
      systemProgram: SystemProgram.programId,
      mplCoreProgram: MPL_CORE_PROGRAM_ID,
    })
    .rpc();
    console.log("\nYour transaction signature", tx);
  });

  it("Try to unstake an NFT before the freeze period ends", async () => {
    // Get the user rewards ATA account
    const userRewardsAta = getAssociatedTokenAddressSync(rewardsMint, provider.wallet.publicKey, false, TOKEN_PROGRAM_ID, ASSOCIATED_TOKEN_PROGRAM_ID);
    try {
      const tx = await program.methods.unstake()
      .accountsPartial({
        owner: provider.wallet.publicKey,
        updateAuthority,
        config,
        rewardsMint,
        userRewardsAta,
        asset: nftKeypair.publicKey,
        collection: collectionKeypair.publicKey,
        mplCoreProgram: MPL_CORE_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      })
      .rpc();
      throw new Error(`Unstake should have failed before freeze period elapsed, but succeeded with tx: ${tx}`);
    } catch (err) {
      if (err instanceof anchor.AnchorError && err.error.errorCode.code === "FreezePeriodNotElapsed") {
        console.log("\nUnstake failed as expected:", err.error.errorMessage);
      } else {
        throw err;
      }
    }
  });

 
  it("Claim rewards while keeping the NFT staked and frozen", async () => {
    const assetBeforeInfo = await provider.connection.getAccountInfo(
      nftKeypair.publicKey,
      "confirmed"
    );
    assert.isNotNull(assetBeforeInfo);
  
    const assetBefore = deserializeAssetV1({
      publicKey: publicKey(nftKeypair.publicKey.toBase58()),
      owner: publicKey(assetBeforeInfo!.owner.toBase58()),
      lamports: lamports(assetBeforeInfo!.lamports),
      executable: assetBeforeInfo!.executable,
      data: assetBeforeInfo!.data,
    });
  
    const attributesBefore = assetBefore.attributes?.attributeList ?? [];
    const stakedAtBefore = attributesBefore.find(
      (attribute) => attribute.key === "staked_at"
    )?.value;
    const checkpointBefore = attributesBefore.find(
      (attribute) => attribute.key === "last_claimed_at"
    )?.value;
  
    assert.isDefined(stakedAtBefore);
    assert.isDefined(checkpointBefore);
    assert.equal(
      attributesBefore.find((attribute) => attribute.key === "staked")?.value,
      "true"
    );
    assert.isTrue(assetBefore.freezeDelegate?.frozen);
  
    // Advance three days from the current on-chain clock.
    const clock = await provider.connection.getAccountInfo(
      SYSVAR_CLOCK_PUBKEY,
      "confirmed"
    );
    assert.isNotNull(clock);
  
    const chainTimestamp = Number(clock!.data.readBigInt64LE(32));
  
    await advanceTime({
      absoluteTimestamp:
        chainTimestamp * 1000 + 3 * MILLISECONDS_PER_DAY,
    });
  
    const userRewardsAta = getAssociatedTokenAddressSync(
      rewardsMint,
      provider.wallet.publicKey,
      false,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    );
  
    const ataBefore = await provider.connection.getAccountInfo(
      userRewardsAta,
      "confirmed"
    );
  
    const balanceBefore = ataBefore
      ? Number(
          (
            await provider.connection.getTokenAccountBalance(
              userRewardsAta,
              "confirmed"
            )
          ).value.amount
        )
      : 0;
  
    await program.methods
      .claimRewards()
      .accountsPartial({
        owner: provider.wallet.publicKey,
        config,
        asset: nftKeypair.publicKey,
        collection: collectionKeypair.publicKey,
        updateAuthority,
        rewardsMint,
        userRewardsAta,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        mplCoreProgram: MPL_CORE_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
  
    const balanceAfter = Number(
      (
        await provider.connection.getTokenAccountBalance(
          userRewardsAta,
          "confirmed"
        )
      ).value.amount
    );
  
    // REWARDS_BPS = 10,000 and decimals = 6:
    // three days earn 3,000,000 base units, or three tokens.
    assert.equal(balanceAfter - balanceBefore, 3_000_000);
  
    const assetAfterInfo = await provider.connection.getAccountInfo(
      nftKeypair.publicKey,
      "confirmed"
    );
    assert.isNotNull(assetAfterInfo);
  
    const assetAfter = deserializeAssetV1({
      publicKey: publicKey(nftKeypair.publicKey.toBase58()),
      owner: publicKey(assetAfterInfo!.owner.toBase58()),
      lamports: lamports(assetAfterInfo!.lamports),
      executable: assetAfterInfo!.executable,
      data: assetAfterInfo!.data,
    });
  
    const attributesAfter = assetAfter.attributes?.attributeList ?? [];
    const getAttribute = (key: string) =>
      attributesAfter.find((attribute) => attribute.key === key)?.value;
  
    // Claiming preserves ownership, staking status, and the freeze.
    assert.equal(
      assetAfter.owner.toString(),
      provider.wallet.publicKey.toBase58()
    );
    assert.equal(getAttribute("staked"), "true");
    assert.isTrue(assetAfter.freezeDelegate?.frozen);
  
    // Preserve the original staking time.
    assert.equal(getAttribute("staked_at"), stakedAtBefore);
  
    // Advance the reward checkpoint by exactly three paid days.
    assert.equal(
      getAttribute("last_claimed_at"),
      String(Number(checkpointBefore!) + 3 * 86_400)
    );
  });
  it("An immediate second claim pays no additional rewards", async () => {
    const userRewardsAta = getAssociatedTokenAddressSync(
      rewardsMint,
      provider.wallet.publicKey,
      false,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    );
  
    async function readAsset() {
      const info = await provider.connection.getAccountInfo(
        nftKeypair.publicKey,
        "confirmed"
      );
      assert.isNotNull(info);
  
      return deserializeAssetV1({
        publicKey: publicKey(nftKeypair.publicKey.toBase58()),
        owner: publicKey(info!.owner.toBase58()),
        lamports: lamports(info!.lamports),
        executable: info!.executable,
        data: info!.data,
      });
    }
  
    const assetBefore = await readAsset();
    const checkpointBefore = assetBefore.attributes?.attributeList.find(
      (attribute) => attribute.key === "last_claimed_at"
    )?.value;
    assert.isDefined(checkpointBefore);
  
    const balanceBefore = (
      await provider.connection.getTokenAccountBalance(
        userRewardsAta,
        "confirmed"
      )
    ).value.amount;
  
    await program.methods
      .claimRewards()
      .accountsPartial({
        owner: provider.wallet.publicKey,
        config,
        asset: nftKeypair.publicKey,
        collection: collectionKeypair.publicKey,
        updateAuthority,
        rewardsMint,
        userRewardsAta,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        mplCoreProgram: MPL_CORE_PROGRAM_ID,
      })
      // Make this transaction distinct from the first claim,
      // even if both use the same recent blockhash.
      .preInstructions([
        ComputeBudgetProgram.setComputeUnitLimit({ units: 250_000 }),
      ])
      .rpc({ commitment: "confirmed" });
  
    const balanceAfter = (
      await provider.connection.getTokenAccountBalance(
        userRewardsAta,
        "confirmed"
      )
    ).value.amount;
  
    const assetAfter = await readAsset();
    const getAttribute = (key: string) =>
      assetAfter.attributes?.attributeList.find(
        (attribute) => attribute.key === key
      )?.value;
  
    assert.equal(balanceAfter, balanceBefore);
    assert.equal(getAttribute("last_claimed_at"), checkpointBefore);
    assert.equal(getAttribute("staked_at"), assetBefore.attributes?.attributeList.find(
      (attribute) => attribute.key === "staked_at"
    )?.value);
    assert.equal(getAttribute("staked"), "true");
    assert.isTrue(assetAfter.freezeDelegate?.frozen);
  });
  it("Time travel to the future", async () => {
    const clockBefore = await provider.connection.getAccountInfo(
      SYSVAR_CLOCK_PUBKEY,
      "confirmed"
    );
  
    if (!clockBefore) {
      throw new Error("Clock sysvar not found");
    }
  
    // The Clock account stores unix_timestamp at byte offset 32.
    // Its value is in seconds.
    const chainTimestamp = Number(
      clockBefore.data.readBigInt64LE(32)
    );
  
    // Surfpool's absoluteTimestamp expects milliseconds.
    const targetTimestamp =
      chainTimestamp * 1000 +
      TIME_TRAVEL_IN_DAYS * MILLISECONDS_PER_DAY;
  
    await advanceTime({
      absoluteTimestamp: targetTimestamp,
    });
  
    const clockAfter = await provider.connection.getAccountInfo(
      SYSVAR_CLOCK_PUBKEY,
      "confirmed"
    );
  
    if (!clockAfter) {
      throw new Error("Clock sysvar not found after time travel");
    }
  
    const updatedTimestamp = Number(
      clockAfter.data.readBigInt64LE(32)
    );
  
    if (updatedTimestamp < targetTimestamp / 1000) {
      throw new Error("On-chain clock did not reach the requested time");
    }
  
    console.log(
      "On-chain days advanced:",
      (updatedTimestamp - chainTimestamp) / 86_400
    );
  });

  
  it("Unstake an NFT", async () => {
    // Get the user rewards ATA account
    const userRewardsAta = getAssociatedTokenAddressSync(rewardsMint, provider.wallet.publicKey, false, TOKEN_PROGRAM_ID, ASSOCIATED_TOKEN_PROGRAM_ID);
    const balanceBefore = Number(
      (
        await provider.connection.getTokenAccountBalance(
          userRewardsAta,
          "confirmed"
        )
      ).value.amount
    );
    
    assert.equal(balanceBefore, 3_000_000);
    
    const tx = await program.methods.unstake()
    .accountsPartial({
      owner: provider.wallet.publicKey,
      updateAuthority,
      config,
      rewardsMint,
      userRewardsAta,
      asset: nftKeypair.publicKey,
      collection: collectionKeypair.publicKey,
      mplCoreProgram: MPL_CORE_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      tokenProgram: TOKEN_PROGRAM_ID,
      associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
    })
      .rpc();

    const balanceAfter = Number(
      (
        await provider.connection.getTokenAccountBalance(
          userRewardsAta,
          "confirmed"
        )
      ).value.amount
    );
    
    // Unstake pays only the five days since the claim checkpoint.
    assert.equal(balanceAfter - balanceBefore, 5_000_000);
    
    // Three tokens claimed earlier + five paid now.
    assert.equal(balanceAfter, 8_000_000);
    console.log("\nYour transaction signature", tx);
    console.log("User rewards balance", (await provider.connection.getTokenAccountBalance(userRewardsAta)).value.uiAmount);
  });

  it("Create and stake a separate NFT for burning", async () => {
    await program.methods
      .mintAsset(
        "Burn Test NFT",
        "https://example.com/burn-test-nft.json"
      )
      .accountsPartial({
        user: provider.wallet.publicKey,
        asset: burnNftKeypair.publicKey,
        collection: collectionKeypair.publicKey,
        updateAuthority,
        systemProgram: SystemProgram.programId,
        mplCoreProgram: MPL_CORE_PROGRAM_ID,
      })
      .signers([burnNftKeypair])
      .rpc({ commitment: "confirmed" });
  
    await program.methods
      .stake()
      .accountsPartial({
        owner: provider.wallet.publicKey,
        config,
        asset: burnNftKeypair.publicKey,
        collection: collectionKeypair.publicKey,
        updateAuthority,
        systemProgram: SystemProgram.programId,
        mplCoreProgram: MPL_CORE_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
  
    const info = await provider.connection.getAccountInfo(
      burnNftKeypair.publicKey,
      "confirmed"
    );
    assert.isNotNull(info);
  
    const asset = deserializeAssetV1({
      publicKey: publicKey(burnNftKeypair.publicKey.toBase58()),
      owner: publicKey(info!.owner.toBase58()),
      lamports: lamports(info!.lamports),
      executable: info!.executable,
      data: info!.data,
    });
  
    const stakingStatus = asset.attributes?.attributeList.find(
      (attribute) => attribute.key === "staked"
    )?.value;
  
    assert.equal(stakingStatus, "true");
    assert.isTrue(asset.freezeDelegate?.frozen);
  });
  it("Reject burning before the freeze period without changing state", async () => {
    const userRewardsAta = getAssociatedTokenAddressSync(
      rewardsMint,
      provider.wallet.publicKey,
      false,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    );
  
    const assetBefore = await provider.connection.getAccountInfo(
      burnNftKeypair.publicKey,
      "confirmed"
    );
    assert.isNotNull(assetBefore);
  
    // The earlier claim tests already created this ATA.
    const balanceBefore = (
      await provider.connection.getTokenAccountBalance(
        userRewardsAta,
        "confirmed"
      )
    ).value.amount;
  
    try {
      await program.methods
        .burnStakedNft()
        .accountsPartial({
          owner: provider.wallet.publicKey,
          config,
          asset: burnNftKeypair.publicKey,
          collection: collectionKeypair.publicKey,
          updateAuthority,
          rewardsMint,
          userRewardsAta,
          tokenProgram: TOKEN_PROGRAM_ID,
          associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          mplCoreProgram: MPL_CORE_PROGRAM_ID,
        })
        .rpc({ commitment: "confirmed" });
  
      assert.fail("Burn should fail before the minimum staking period");
    } catch (error) {
      if (!(error instanceof anchor.AnchorError)) {
        throw error;
      }
  
      assert.equal(
        error.error.errorCode.code,
        "FreezePeriodNotElapsed"
      );
    }
  
    const assetAfter = await provider.connection.getAccountInfo(
      burnNftKeypair.publicKey,
      "confirmed"
    );
    assert.isNotNull(assetAfter);
  
    // All NFT data, including staking attributes and freeze state,
    // must remain unchanged.
    assert.isTrue(assetAfter!.data.equals(assetBefore!.data));
    assert.equal(
      assetAfter!.owner.toBase58(),
      assetBefore!.owner.toBase58()
    );
  
    const balanceAfter = (
      await provider.connection.getTokenAccountBalance(
        userRewardsAta,
        "confirmed"
      )
    ).value.amount;
  
    assert.equal(balanceAfter, balanceBefore);
  });

  it("Burn a staked NFT and receive unpaid rewards plus the bonus", async () => {
    // Advance eight days from the current on-chain clock.
    const clock = await provider.connection.getAccountInfo(
      SYSVAR_CLOCK_PUBKEY,
      "confirmed"
    );
    assert.isNotNull(clock);
  
    const chainTimestamp = Number(clock!.data.readBigInt64LE(32));
  
    await advanceTime({
      absoluteTimestamp:
        chainTimestamp * 1000 + 8 * MILLISECONDS_PER_DAY,
    });
  
    const userRewardsAta = getAssociatedTokenAddressSync(
      rewardsMint,
      provider.wallet.publicKey,
      false,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    );
  
    const balanceBefore = Number(
      (
        await provider.connection.getTokenAccountBalance(
          userRewardsAta,
          "confirmed"
        )
      ).value.amount
    );
  
    const tx = await program.methods
      .burnStakedNft()
      .accountsPartial({
        owner: provider.wallet.publicKey,
        config,
        asset: burnNftKeypair.publicKey,
        collection: collectionKeypair.publicKey,
        updateAuthority,
        rewardsMint,
        userRewardsAta,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        mplCoreProgram: MPL_CORE_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
  
    const balanceAfter = Number(
      (
        await provider.connection.getTokenAccountBalance(
          userRewardsAta,
          "confirmed"
        )
      ).value.amount
    );
  
    // Eight unpaid days at one token per day, plus a 100-token bonus.
    assert.equal(balanceAfter - balanceBefore, 108_000_000);
  
    const burnedAccount = await provider.connection.getAccountInfo(
      burnNftKeypair.publicKey,
      "confirmed"
    );
  
    // Core may retain an Uninitialized tombstone instead of removing
    // the account. It must no longer contain a live NFT.
    if (burnedAccount !== null) {
      assert.equal(burnedAccount.data.length, 1);
      assert.equal(burnedAccount.data[0], 0);
    }
  
    console.log("Burn transaction:", tx);
    console.log(
      "Tokens received:",
      (balanceAfter - balanceBefore) / 1_000_000
    );
  });

  it("Reject a second burn without paying another bonus", async () => {
    const userRewardsAta = getAssociatedTokenAddressSync(
      rewardsMint,
      provider.wallet.publicKey,
      false,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    );
  
    const balanceBefore = (
      await provider.connection.getTokenAccountBalance(
        userRewardsAta,
        "confirmed"
      )
    ).value.amount;
  
    let rejected = false;
  
    try {
      await program.methods
        .burnStakedNft()
        .accountsPartial({
          owner: provider.wallet.publicKey,
          config,
          asset: burnNftKeypair.publicKey,
          collection: collectionKeypair.publicKey,
          updateAuthority,
          rewardsMint,
          userRewardsAta,
          tokenProgram: TOKEN_PROGRAM_ID,
          associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          mplCoreProgram: MPL_CORE_PROGRAM_ID,
        })
        // Distinguish this transaction from the first burn,
        // even if the recent blockhash is unchanged.
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 250_000 }),
        ])
        .rpc({ commitment: "confirmed" });
    }  catch (error) {
      if (error instanceof anchor.AnchorError) {
        assert.equal(error.error.origin, "asset");
      } else if (error instanceof SendTransactionError) {
        const logs =
          error.logs ?? await error.getLogs(provider.connection);
    
        const failedOnBurnedAsset = logs.some(
          (line) =>
            line.includes("ProgramError caused by account: asset") &&
            line.includes('BorshIoError("Unexpected length of input")')
        );
    
        assert.isTrue(
          failedOnBurnedAsset,
          "Expected rejection while deserializing the burned asset"
        );
      } else {
        throw error;
      }
    
      rejected = true;
    }
  
    assert.isTrue(rejected, "A burned NFT must not be accepted again");
  
    const balanceAfter = (
      await provider.connection.getTokenAccountBalance(
        userRewardsAta,
        "confirmed"
      )
    ).value.amount;
  
    assert.equal(balanceAfter, balanceBefore);
  });

});


