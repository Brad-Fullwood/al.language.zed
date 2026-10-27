page 50100 "FX Page"
{
    PageType = List;
    SourceTable = "FX Base";
    Caption = 'FX Page';

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("No."; Rec."No.")
                {
                    ApplicationArea = All;
                    ToolTip = 'Specifies the number.';
                }
                field(Name; Rec.Name)
                {
                    ApplicationArea = All;
                    Caption = 'The name';
                }
            }
        }
    }

    actions
    {
        area(Processing)
        {
            action(DoIt)
            {
                ApplicationArea = All;
                Caption = 'Do it';
                ToolTip = 'Does it.';
            }
            group(More)
            {
                Caption = 'More';

                action(Inner)
                {
                    ApplicationArea = All;
                }
            }
        }
    }
}
