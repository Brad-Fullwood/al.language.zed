report 50100 "FX Report"
{
    Caption = 'FX Report';
    ProcessingOnly = true;

    dataset
    {
        dataitem(Cust; Customer)
        {
            RequestFilterHeading = 'Customer filter';

            column(CustNo; "No.")
            {
                Caption = 'Cust No';
            }
            column(CustName; Name)
            {
            }
        }
    }

    requestpage
    {
        layout
        {
            area(Content)
            {
                group(Options)
                {
                    Caption = 'Options';

                    field(ShowAll; ShowAllVar)
                    {
                        ApplicationArea = All;
                        Caption = 'Show all';
                    }
                }
            }
        }
    }

    labels
    {
        FXLabel = 'Label text';
    }

    var
        ShowAllVar: Boolean;
}
