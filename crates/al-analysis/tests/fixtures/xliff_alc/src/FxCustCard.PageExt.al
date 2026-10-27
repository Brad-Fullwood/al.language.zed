pageextension 50101 "FX Cust Card B" extends "Customer Card"
{
    actions
    {
        addlast(Processing)
        {
            action(FXActB)
            {
                ApplicationArea = All;
            }
        }
    }

    var
        PBLbl: Label 'Page B label';
}

pageextension 50100 "FX Cust Card A" extends "Customer Card"
{
    layout
    {
        addlast(General)
        {
            field("FX A Code"; Rec."FX A Code")
            {
                ApplicationArea = All;
                Caption = 'A Code Page';
                ToolTip = 'Specifies A.';
            }
        }
        modify("No.")
        {
            ToolTip = 'Modified tooltip';
        }
    }

    actions
    {
        addlast(Processing)
        {
            action(FXActA)
            {
                ApplicationArea = All;
                Caption = 'FX A';
                ToolTip = 'Run A.';
            }
        }
    }
}
