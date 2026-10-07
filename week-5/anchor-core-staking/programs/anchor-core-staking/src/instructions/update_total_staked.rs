use anchor_lang::prelude::*;
use mpl_core::{
    accounts::BaseCollectionV1,
    fetch_plugin,
    instructions::UpdateCollectionPluginV1CpiBuilder,
    types::{Attributes, Plugin, PluginType},
};

use crate::error::ErrorCode;

pub enum StakedCountChange {
    Increment,
    Decrement,
}

pub fn update_total_staked<'info>(
    collection: &AccountInfo<'info>,
    update_authority: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
    change: StakedCountChange,
) -> Result<()> {
    let (_, mut attributes, _) =
        fetch_plugin::<BaseCollectionV1, Attributes>(
            collection,
            PluginType::Attributes,
        )
        .map_err(|_| ErrorCode::InvalidStakingCounter)?;

    // Require exactly one counter entry.
    let counter_entries = attributes
        .attribute_list
        .iter()
        .filter(|attribute| attribute.key == "total_staked")
        .count();

    require!(
        counter_entries == 1,
        ErrorCode::InvalidStakingCounter
    );

    let counter = attributes
        .attribute_list
        .iter_mut()
        .find(|attribute| attribute.key == "total_staked")
        .ok_or(ErrorCode::InvalidStakingCounter)?;

    let current = counter
        .value
        .parse::<u64>()
        .map_err(|_| ErrorCode::InvalidStakingCounter)?;

    let updated = match change {
        StakedCountChange::Increment => current.checked_add(1),
        StakedCountChange::Decrement => current.checked_sub(1),
    }
    .ok_or(ErrorCode::InvalidStakingCounter)?;

    counter.value = updated.to_string();

    UpdateCollectionPluginV1CpiBuilder::new(mpl_core_program)
        .collection(collection)
        .payer(payer)
        .authority(Some(update_authority))
        .system_program(system_program)
        .plugin(Plugin::Attributes(attributes))
        .invoke_signed(signer_seeds)?;

    Ok(())
}