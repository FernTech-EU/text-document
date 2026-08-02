//! SetListFormat tests

extern crate text_document_formatting as document_formatting;

use anyhow::Result;

use document_formatting::{ListStyle, SetListFormatDto};
use document_formatting::document_formatting_controller;

use test_harness::{create_list, list_controller, setup_with_text};

#[test]
fn test_set_list_format_change_style() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("One\nTwo\nThree")?;
    let list_result = create_list(
        &db,
        &hub,
        &mut urm,
        0,
        13,
        common::entities::ListStyle::Disc,
    )?;
    let list_id = list_result.list_id;

    let list = list_controller::get(&db, &list_id)?.unwrap();
    assert_eq!(list.style, common::entities::ListStyle::Disc);

    document_formatting_controller::set_list_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetListFormatDto {
            list_id: list_id as i64,
            style: Some(ListStyle::Decimal),
            indent: None,
            prefix: None,
            suffix: None,
        },
    )?;

    let list = list_controller::get(&db, &list_id)?.unwrap();
    assert_eq!(list.style, common::entities::ListStyle::Decimal);
    Ok(())
}

#[test]
fn test_set_list_format_all_fields() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("A\nB")?;
    let list_result = create_list(&db, &hub, &mut urm, 0, 3, common::entities::ListStyle::Disc)?;
    let list_id = list_result.list_id;

    document_formatting_controller::set_list_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetListFormatDto {
            list_id: list_id as i64,
            style: Some(ListStyle::UpperRoman),
            indent: Some(3),
            prefix: Some("(".into()),
            suffix: Some(")".into()),
        },
    )?;

    let list = list_controller::get(&db, &list_id)?.unwrap();
    assert_eq!(list.style, common::entities::ListStyle::UpperRoman);
    assert_eq!(list.indent, 3);
    assert_eq!(list.prefix, "(");
    assert_eq!(list.suffix, ")");
    Ok(())
}

#[test]
fn test_set_list_format_preserves_unset_fields() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("X\nY")?;
    let list_result = create_list(
        &db,
        &hub,
        &mut urm,
        0,
        3,
        common::entities::ListStyle::Square,
    )?;
    let list_id = list_result.list_id;

    document_formatting_controller::set_list_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetListFormatDto {
            list_id: list_id as i64,
            prefix: Some("[".into()),
            suffix: Some("]".into()),
            ..Default::default()
        },
    )?;
    document_formatting_controller::set_list_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetListFormatDto {
            list_id: list_id as i64,
            indent: Some(5),
            ..Default::default()
        },
    )?;

    let list = list_controller::get(&db, &list_id)?.unwrap();
    assert_eq!(
        list.style,
        common::entities::ListStyle::Square,
        "Style should be preserved"
    );
    assert_eq!(list.prefix, "[", "Prefix should be preserved");
    assert_eq!(list.suffix, "]", "Suffix should be preserved");
    assert_eq!(list.indent, 5);
    Ok(())
}

#[test]
fn test_set_list_format_undo_redo() -> Result<()> {
    let (db, hub, mut urm) = setup_with_text("Item")?;
    let list_result = create_list(&db, &hub, &mut urm, 0, 4, common::entities::ListStyle::Disc)?;
    let list_id = list_result.list_id;

    document_formatting_controller::set_list_format(
        &db,
        &hub,
        &mut urm,
        None,
        &SetListFormatDto {
            list_id: list_id as i64,
            style: Some(ListStyle::LowerAlpha),
            indent: Some(4),
            prefix: Some(">> ".into()),
            suffix: None,
        },
    )?;

    let list = list_controller::get(&db, &list_id)?.unwrap();
    assert_eq!(list.style, common::entities::ListStyle::LowerAlpha);
    assert_eq!(list.indent, 4);
    assert_eq!(list.prefix, ">> ");

    urm.undo(None)?;
    let list = list_controller::get(&db, &list_id)?.unwrap();
    assert_eq!(list.style, common::entities::ListStyle::Disc);
    assert_eq!(list.indent, 0);

    urm.redo(None)?;
    let list = list_controller::get(&db, &list_id)?.unwrap();
    assert_eq!(list.style, common::entities::ListStyle::LowerAlpha);
    assert_eq!(list.indent, 4);
    Ok(())
}
