table 50100 "Fix Header"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { Clustered = true; } }
}
table 50101 "Fix Line"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { Clustered = true; } }
}
page 50100 "Fix Card"
{
    PageType = Card;
    SourceTable = "Fix Header";
    layout { area(Content) { field("No."; Rec."No.") { ApplicationArea = All; } } }
}
codeunit 50100 "Fix Mgt"
{
    procedure Run2() begin end;
}
report 50100 "Fix Report"
{
    dataset { dataitem(H; "Fix Header") { column(No; "No.") { } } }
}
xmlport 50100 "Fix Port"
{
    schema { textelement(Root) { tableelement(H; "Fix Header") { fieldelement(No; H."No.") { } } } }
}
query 50100 "Fix Query"
{
    elements { dataitem(H; "Fix Header") { column(No; "No.") { } } }
}
