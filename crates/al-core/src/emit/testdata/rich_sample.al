enum 50100 "Spike Color"
{
    Extensible = true;
    value(0; Red) { Caption = 'Red'; }
    value(1; Green) { }
}

interface "Spike IFoo"
{
    procedure Foo(x: Integer): Boolean;
}

table 50100 "Spike Rec"
{
    DataClassification = CustomerContent;
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Name; Text[100]) { Caption = 'Name'; }
        field(3; Amount; Decimal) { }
        field(4; Color; Enum "Spike Color") { }
        field(5; Status; Option) { OptionMembers = " ",Open,Closed; OptionCaption = ' ,Open,Closed'; }
        field(6; "Parent No."; Integer) { TableRelation = "Spike Rec"."Entry No."; }
    }
    keys { key(PK; "Entry No.") { Clustered = true; } }
    fieldgroups { fieldgroup(Brick; "Entry No.", Name) { } }
    procedure Helper(): Integer begin exit(1); end;
}

codeunit 50100 "Spike Hello" implements "Spike IFoo"
{
    procedure Foo(x: Integer): Boolean begin exit(true); end;
    procedure Greet(name: Text): Text begin exit('hi'); end;
    procedure ProcessRec(var Rec: Record "Spike Rec"; Col: Enum "Spike Color"): Boolean begin exit(true); end;
    procedure WithIface(f: Interface "Spike IFoo") begin end;
    [IntegrationEvent(false, false)]
    procedure OnDidThing(var Rec: Record "Spike Rec") begin end;
    [TryFunction]
    procedure TryThing() begin end;
    local procedure InternalOnly() begin end;
}

tableextension 50101 "Spike Rec Ext" extends "Spike Rec"
{
    fields { field(50100; MoreInfo; Text[50]) { Caption='More Info'; } }
}

enumextension 50101 "Spike Color Ext" extends "Spike Color" { value(2; Blue){ Caption='Blue'; } }

query 50100 "Spike Query"
{
    QueryType = Normal;
    elements { dataitem(Rec; "Spike Rec") { column(EntryNo; "Entry No."){} column(Amt; Amount){} } }
}

permissionset 50100 "Spike Perms"
{
    Assignable = true;
    Caption = 'Spike Perms';
    Permissions = tabledata "Spike Rec" = RIMD, table "Spike Rec" = X, codeunit "Spike Hello" = X, system "Tools, Object Designer" = X;
}

page 50100 "Spike Card"
{
    PageType = Card;
    SourceTable = "Spike Rec";
    Caption = 'Spike Card';
    layout { area(Content) { group(General) { field("Entry No."; Rec."Entry No."){ ApplicationArea=All; } field(Amount; Rec.Amount){ ApplicationArea=All; } } } }
    actions { area(Processing) { action(Refresh) { ApplicationArea=All; } } }
}

report 50100 "Spike Report"
{
    Caption = 'Spike Report';
    DefaultRenderingLayout = SpikeLayout;
    dataset { dataitem(Data; "Spike Rec") { column(EntryNo; "Entry No."){} column(Amt; Amount){} } }
    requestpage { layout { area(Content) { field(ShowAll; ShowAll){ ApplicationArea=All; } } } }
    rendering { layout(SpikeLayout) { Type = RDLC; LayoutFile = 'src/spike.rdl'; } }
    var ShowAll: Boolean;
}

page 50101 "Spike RC" { PageType = RoleCenter; }
controladdin "Spike Addin" { RequestedHeight = 100; MinimumHeight = 50; }
pageextension 50102 "Spike Card Ext" extends "Spike Card"
{
    layout { addlast(Content) { field(Extra; Rec.Name){ ApplicationArea=All; } } }
}
profile "Spike Profile" { Caption='Spike Profile'; RoleCenter = "Spike RC"; }
profileextension "Spike Profile Ext" extends "Spike Profile" { Caption = 'Spike Profile Ext'; }
pagecustomization "Spike Card Cust" customizes "Spike Card"
{
    layout { addlast(General) { field(NameC; Rec.Name){ ApplicationArea=All; } } }
}
xmlport 50100 "Spike Xml" { schema { textelement(Root) { tableelement(R; "Spike Rec") { fieldelement(EntryNo; R."Entry No.") {} } } } }
permissionsetextension 50101 "Spike Perms Ext" extends "Spike Perms" { Permissions = tabledata "Spike Rec" = D; }
reportextension 50103 "Spike Report Ext" extends "Spike Report"
{
    dataset { add(Data) { column(NameCol; Name) { } } }
    requestpage { layout { addlast(Content) { field(MoreOpt; MoreOpt){ ApplicationArea=All; } } } }
    var MoreOpt: Integer;
}
