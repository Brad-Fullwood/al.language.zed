report 50120 "Customer List"
{
    UsageCategory = ReportsAndAnalysis;
    ApplicationArea = All;
    DefaultLayout = Word;

    dataset
    {
        dataitem(Customer; "Test Customer")
        {
            column(No; "No.") { }
            column(Name; Name) { }
            column(Balance; Balance) { }
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
                    field(ShowDetails; ShowDetails)
                    {
                        ApplicationArea = All;
                        Caption = 'Show Details';
                    }
                }
            }
        }
    }

    var
        ShowDetails: Boolean;
}
